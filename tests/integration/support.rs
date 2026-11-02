//! Shared test helpers.

use std::future::Future;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use autumn_plugin_bevy::prelude::*;
use autumn_plugin_bevy::{BevyConfig, bevy_app::App, bevy_ecs::prelude::*};

/// A marker component.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Marker;

/// A value component.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Value(pub u64);

/// The count of `Update` runs.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Updates(pub u64);

/// An ordered log of numbers.
#[derive(Resource, Debug, Default, Clone, PartialEq, Eq)]
pub struct Log(pub Vec<u32>);

/// Counts the `Update` runs.
pub fn count_updates(mut updates: ResMut<Updates>) {
    updates.0 += 1;
}

/// Manual mode with a long command timeout.
pub fn manual() -> BevyConfig {
    BevyConfig::manual()
        .with_command_timeout(Some(Duration::from_secs(10)))
        .with_shutdown_timeout(Duration::from_secs(5))
}

/// A setup that counts updates.
pub fn counting(app: &mut App) {
    app.init_resource::<Updates>()
        .init_resource::<Log>()
        .add_systems(bevy_app::Update, count_updates);
}

/// Starts `runtime` and waits until it is ready.
pub async fn ready(runtime: BevyRuntime) -> BevyRuntime {
    runtime.start().expect("start");
    runtime.wait_ready().await.expect("ready");
    runtime
}

/// Polls `cond` until it is `true` or `limit` passes.
pub async fn eventually(limit: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    cond()
}

/// Runs `f` on a new thread and fails the test when it does not end in
/// `limit`. Use it where a hang would block the test thread.
pub fn within<T: Send + 'static>(limit: Duration, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)));
    });
    match rx.recv_timeout(limit) {
        Ok(Ok(value)) => value,
        Ok(Err(panic)) => std::panic::resume_unwind(panic),
        Err(_) => panic!("watchdog: the test did not end within {limit:?}"),
    }
}

/// Runs `fut` to the end on a new current-thread runtime, under a watchdog.
pub fn current_thread<F, Fut>(limit: Duration, f: F) -> Fut::Output
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future,
    Fut::Output: Send + 'static,
{
    within(limit, move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(f())
    })
}

/// A gate that holds the Bevy thread inside a command until `open`.
pub struct Gate {
    entered_rx: mpsc::Receiver<()>,
    open_tx: mpsc::Sender<()>,
}

impl Gate {
    /// Sends a command that blocks the Bevy thread. Returns the gate and the
    /// task of the blocked command. The task resolves after `open`.
    pub fn hold(handle: &BevyHandle) -> (Self, tokio::task::JoinHandle<Result<(), RuntimeError>>) {
        Self::hold_after(handle, |_world| {})
    }

    /// The same as [`Gate::hold`], but the command first runs `before` on the
    /// world.
    pub fn hold_after(
        handle: &BevyHandle,
        before: impl FnOnce(&mut World) + Send + 'static,
    ) -> (Self, tokio::task::JoinHandle<Result<(), RuntimeError>>) {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (open_tx, open_rx) = mpsc::channel::<()>();
        let handle = handle.clone();
        let task = tokio::spawn(async move {
            handle
                .with_world_timeout(None, move |world| {
                    before(world);
                    let _ = entered_tx.send(());
                    let _ = open_rx.recv_timeout(Duration::from_secs(20));
                })
                .await
        });
        (
            Self {
                entered_rx,
                open_tx,
            },
            task,
        )
    }

    /// Waits until the Bevy thread is inside the gate.
    pub fn wait_entered(&self) {
        self.entered_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the gate command did not start");
    }

    /// Lets the Bevy thread go on.
    pub fn open(&self) {
        let _ = self.open_tx.send(());
    }
}

/// Waits for the gate on a blocking thread, so the test runtime keeps
/// running.
pub async fn entered(gate: Gate) -> Gate {
    tokio::task::spawn_blocking(move || {
        gate.wait_entered();
        gate
    })
    .await
    .expect("join")
}
