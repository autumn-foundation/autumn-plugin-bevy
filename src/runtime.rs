//! [`BevyRuntime`]: the owner of the Bevy thread.

use std::fmt;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};

use crate::Setup;
use crate::command::Msg;
use crate::config::BevyConfig;
use crate::driver;
use crate::error::RuntimeError;
use crate::handle::BevyHandle;
use crate::health::BevyHealth;
use crate::lifecycle::Event;
use crate::metrics::BevyMetrics;
use crate::shared::{Failure, Shared};
use crate::status::{RuntimeId, RuntimeStatus};

/// The parts that `start` moves to the thread.
struct Pending {
    setup: Setup,
    rx: Receiver<Msg>,
}

/// One headless Bevy app on its own thread.
///
/// [`BevyPlugin`](crate::BevyPlugin) makes one for the Autumn app. Tests,
/// jobs and command-line tools can use it with no Autumn app.
///
/// The thread stops on [`BevyRuntime::shutdown`], on `AppExit`, on a panic in
/// an update, or when the runtime and all its handles are dropped. A live
/// [`BevyHandle`] keeps the thread running.
pub struct BevyRuntime {
    shared: Arc<Shared>,
    tx: SyncSender<Msg>,
    pending: Mutex<Option<Pending>>,
}

impl BevyRuntime {
    /// Makes a runtime that is not started. `setup` runs on the Bevy thread
    /// after `start`.
    #[must_use]
    pub fn new<F>(config: BevyConfig, setup: F) -> Self
    where
        F: FnOnce(&mut bevy_app::App) + Send + 'static,
    {
        let (tx, rx) = sync_channel(config.queue_capacity.get());
        let shared = Arc::new(Shared::new(config));
        Self {
            shared,
            tx,
            pending: Mutex::new(Some(Pending {
                setup: Box::new(setup),
                rx,
            })),
        }
    }

    /// Spawns the `autumn-bevy` thread. It does not wait for setup.
    ///
    /// # Errors
    ///
    /// [`RuntimeError::AlreadyStarted`] on a second call,
    /// [`RuntimeError::NotRunning`] after a shutdown,
    /// [`RuntimeError::Reentrant`] on the Bevy thread, and
    /// [`RuntimeError::Spawn`] when the thread does not start.
    pub fn start(&self) -> Result<(), RuntimeError> {
        if driver::on_runtime_thread(self.shared.id) {
            return Err(RuntimeError::Reentrant);
        }
        // Hold the lock until the thread runs, so two calls cannot both start.
        let mut slot = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(Pending { setup, rx }) = slot.take() else {
            return Err(RuntimeError::AlreadyStarted);
        };
        if self.shared.transition(Event::Start).is_none() {
            return Err(RuntimeError::NotRunning(self.shared.status()));
        }
        if cfg!(panic = "abort") {
            tracing::warn!(
                "panic = \"abort\": a panic in a Bevy system or command stops the process"
            );
        }
        driver::spawn(Arc::clone(&self.shared), setup, rx).map_err(|err| {
            self.shared.transition(Event::Fail);
            self.shared.transition(Event::Drained { failed: true });
            RuntimeError::Spawn(err)
        })
    }

    /// Waits until setup and the first update are done.
    ///
    /// It waits on a blocking thread, with no tokio timer, so it also works
    /// inside `TestApp` startup hooks. When the limit passes, the runtime
    /// stops and then has the status `Failed`.
    ///
    /// # Errors
    ///
    /// [`RuntimeError::SetupPanicked`], [`RuntimeError::StartupTimeout`],
    /// [`RuntimeError::NotRunning`] when the runtime is not started or stopped,
    /// and [`RuntimeError::Reentrant`] on the Bevy thread.
    pub async fn wait_ready(&self) -> Result<(), RuntimeError> {
        if driver::on_runtime_thread(self.shared.id) {
            return Err(RuntimeError::Reentrant);
        }
        let status = self.shared.status();
        if status == RuntimeStatus::NotStarted {
            return Err(RuntimeError::NotRunning(status));
        }
        let limit = self.shared.config.startup_timeout;
        let shared = Arc::clone(&self.shared);
        let status =
            blocking(move || shared.wait_until(|s| s != RuntimeStatus::Starting, limit)).await;
        match status {
            RuntimeStatus::Running => Ok(()),
            RuntimeStatus::Starting => {
                // A start that did not end in time must not go on unseen.
                self.shared.set_failure(Failure::StartupTimeout(limit));
                self.shared.request_stop();
                self.wake();
                Err(RuntimeError::StartupTimeout(limit))
            }
            other => Err(match self.shared.failure() {
                Some(Failure::SetupPanicked(message)) => RuntimeError::SetupPanicked(message),
                Some(Failure::StartupTimeout(limit)) => RuntimeError::StartupTimeout(limit),
                _ => RuntimeError::NotRunning(other),
            }),
        }
    }

    /// Stops the runtime and waits for the thread. A second call is a no-op.
    ///
    /// The thread runs one more update with `AppExit::Success`, rejects the
    /// queued commands, and drops the world.
    ///
    /// # Errors
    ///
    /// [`RuntimeError::ShutdownTimeout`] when the thread does not stop in time
    /// (the thread continues and stops later), and [`RuntimeError::Reentrant`]
    /// on the Bevy thread.
    pub async fn shutdown(&self) -> Result<(), RuntimeError> {
        if driver::on_runtime_thread(self.shared.id) {
            return Err(RuntimeError::Reentrant);
        }
        // With no thread, there is nothing to wait for.
        if self.shared.status() == RuntimeStatus::NotStarted
            && self.shared.transition(Event::Stop).is_some()
        {
            return Ok(());
        }
        if self.shared.status().is_terminal() {
            return Ok(());
        }
        self.shared.request_stop();
        self.wake();
        let limit = self.shared.config.shutdown_timeout;
        let shared = Arc::clone(&self.shared);
        let status = blocking(move || shared.wait_until(RuntimeStatus::is_terminal, limit)).await;
        if status.is_terminal() {
            Ok(())
        } else {
            Err(RuntimeError::ShutdownTimeout(limit))
        }
    }

    /// Wakes the loop, so it reads the stop flag now. When the queue is full,
    /// the loop reads the flag after the next message.
    fn wake(&self) {
        self.shared.stats.enqueue();
        if self.tx.try_send(Msg::Wake).is_err() {
            self.shared.stats.dequeue();
        }
    }

    /// A handle for async code.
    #[must_use]
    pub fn handle(&self) -> BevyHandle {
        BevyHandle::new(self.tx.clone(), Arc::clone(&self.shared))
    }

    /// The health indicator for this runtime.
    #[must_use]
    pub fn health(&self) -> BevyHealth {
        BevyHealth::new(Arc::clone(&self.shared))
    }

    /// The metrics source for this runtime.
    #[must_use]
    pub fn metrics(&self) -> BevyMetrics {
        BevyMetrics::new(Arc::clone(&self.shared))
    }

    /// The current status.
    #[must_use]
    pub fn status(&self) -> RuntimeStatus {
        self.shared.status()
    }

    /// The identity of this runtime.
    #[must_use]
    pub fn id(&self) -> RuntimeId {
        self.shared.id
    }

    /// The configuration.
    #[must_use]
    pub fn config(&self) -> &BevyConfig {
        &self.shared.config
    }
}

/// Runs a blocking wait off the tokio worker threads. With no tokio
/// runtime, it waits on this thread.
async fn blocking<T, F>(wait: F) -> T
where
    T: Send + 'static,
    F: Fn() -> T + Send + Sync + 'static,
{
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return wait();
    };
    let wait = Arc::new(wait);
    let on_pool = Arc::clone(&wait);
    match runtime.spawn_blocking(move || on_pool()).await {
        Ok(value) => value,
        // The blocking pool shuts down. The wait has a limit, so do it here.
        Err(_) => wait(),
    }
}

impl fmt::Debug for BevyRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BevyRuntime")
            .field("id", &self.shared.id)
            .field("status", &self.shared.status())
            .finish_non_exhaustive()
    }
}
