//! The loop on the `autumn-bevy` thread. Only this thread touches the
//! `App` and the `World`.
//!
//! Order in each pass: the stop flag, then a due update, then one message.
//! So a flood of commands cannot stop the ticks (invariant I9).

use std::cell::Cell;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use bevy_app::{App, AppExit, PluginsState};

use crate::Setup;
use crate::command::{Msg, panic_message};
use crate::config::{BevyConfig, TickMode};
use crate::error::RuntimeError;
use crate::lifecycle::Event;
use crate::schedule::next_deadline;
use crate::shared::{Failure, Shared, Stats};
use crate::status::{RuntimeId, RuntimeStatus};

/// The name of the Bevy thread.
pub(crate) const THREAD_NAME: &str = "autumn-bevy";

/// The longest wait for a message. It bounds how late the loop sees the stop
/// flag when no message wakes it.
const IDLE_WAKE: Duration = Duration::from_millis(50);

/// The wait between two checks while plugins are not ready.
const PLUGIN_POLL: Duration = Duration::from_millis(1);

thread_local! {
    static CURRENT: Cell<Option<RuntimeId>> = const { Cell::new(None) };
}

/// `true` on the thread of runtime `id`. A call into the same runtime from
/// there would wait for itself.
pub(crate) fn on_runtime_thread(id: RuntimeId) -> bool {
    CURRENT.with(|current| current.get() == Some(id))
}

/// Starts the thread. The thread owns `rx` and the app until it ends.
pub(crate) fn spawn(shared: Arc<Shared>, setup: Setup, rx: Receiver<Msg>) -> io::Result<()> {
    std::thread::Builder::new()
        .name(THREAD_NAME.to_owned())
        .spawn(move || Driver { shared, rx }.run(setup))
        .map(drop)
}

/// What the loop does next.
enum Flow {
    Continue,
    /// A stop request, or a closed channel. A last update runs.
    Stop,
    /// A system wrote `AppExit::Success`.
    Exit,
    Fail(Failure),
}

struct Driver {
    shared: Arc<Shared>,
    rx: Receiver<Msg>,
}

impl Driver {
    fn run(self, setup: Setup) {
        CURRENT.with(|current| current.set(Some(self.shared.id)));
        let (app, flow) = self.boot(setup);
        let (app, flow) = match (app, flow) {
            (Some(mut app), Flow::Continue) => {
                self.shared.transition(Event::Ready);
                tracing::info!(runtime = %self.shared.id, "bevy runtime is running");
                let flow = self.drive(&mut app);
                (Some(app), flow)
            }
            other => other,
        };
        self.finish(app, flow);
    }

    /// Setup, the plugin build steps, and the first update.
    fn boot(&self, setup: Setup) -> (Option<App>, Flow) {
        self.shared.mark_busy();
        let config = &self.shared.config;
        let deadline = Instant::now() + config.startup_timeout;
        let built = catch_unwind(AssertUnwindSafe(|| {
            let mut app = App::new();
            setup(&mut app);
            add_time(&mut app, config);
            app
        }));
        let mut app = match built {
            Ok(app) => app,
            Err(payload) => return (None, setup_panic(payload.as_ref())),
        };
        loop {
            match catch_unwind(AssertUnwindSafe(|| app.plugins_state())) {
                Ok(PluginsState::Adding) => {}
                Ok(PluginsState::Cleaned) => break,
                Ok(PluginsState::Ready | PluginsState::Finished) => {
                    let done = catch_unwind(AssertUnwindSafe(|| {
                        app.finish();
                        app.cleanup();
                    }));
                    if let Err(payload) = done {
                        return (Some(app), setup_panic(payload.as_ref()));
                    }
                    break;
                }
                Err(payload) => return (Some(app), setup_panic(payload.as_ref())),
            }
            if self.shared.stop_requested() {
                return (Some(app), Flow::Stop);
            }
            if Instant::now() >= deadline {
                return (
                    Some(app),
                    Flow::Fail(Failure::StartupTimeout(config.startup_timeout)),
                );
            }
            tick_task_pools();
            std::thread::sleep(PLUGIN_POLL);
        }
        // The boot update runs `Startup`. So handlers see what it made.
        let flow = match self.update(&mut app) {
            Flow::Fail(Failure::UpdatePanicked(message)) => {
                Flow::Fail(Failure::SetupPanicked(message))
            }
            other => other,
        };
        (Some(app), flow)
    }

    /// The main loop. It returns when the runtime must stop.
    fn drive(&self, app: &mut App) -> Flow {
        let period = match self.shared.config.tick_mode {
            TickMode::Fixed(rate) => Some(rate.period()),
            TickMode::Manual => None,
        };
        let mut deadline = period.map(|period| Instant::now() + period);
        loop {
            if self.shared.stop_requested() {
                return Flow::Stop;
            }
            let mut wait = IDLE_WAKE;
            if let (Some(period), Some(due)) = (period, deadline) {
                let now = Instant::now();
                if now >= due {
                    let flow = self.update(app);
                    if !matches!(flow, Flow::Continue) {
                        return flow;
                    }
                    let (next, skipped) = next_deadline(due, period, Instant::now());
                    self.shared
                        .stats
                        .overruns
                        .fetch_add(skipped, Ordering::Relaxed);
                    deadline = Some(next);
                    continue;
                }
                wait = wait.min(due - now);
            }
            self.shared.mark_idle();
            match self.rx.recv_timeout(wait) {
                Ok(msg) => {
                    self.shared.mark_busy();
                    let flow = self.handle(app, msg);
                    if !matches!(flow, Flow::Continue) {
                        return flow;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                // No handle and no runtime is left, so nobody can call.
                Err(RecvTimeoutError::Disconnected) => return Flow::Stop,
            }
        }
    }

    /// Runs one message.
    fn handle(&self, app: &mut App, msg: Msg) -> Flow {
        let stats = &self.shared.stats;
        stats.dequeue();
        match msg {
            Msg::Run(command) => {
                command.run(app.world_mut(), stats);
                Flow::Continue
            }
            Msg::Step { n, reply } => {
                if reply.is_closed() {
                    Stats::add(&stats.commands_cancelled);
                    return Flow::Continue;
                }
                for _ in 0..n {
                    let flow = self.update(app);
                    if !matches!(flow, Flow::Continue) {
                        let _ = reply.send(Err(RuntimeError::NotRunning(RuntimeStatus::Stopping)));
                        return flow;
                    }
                }
                let _ = reply.send(Ok(Stats::get(&stats.ticks)));
                Flow::Continue
            }
            Msg::Wake => Flow::Continue,
        }
    }

    /// One `App::update`, with its panic contained.
    fn update(&self, app: &mut App) -> Flow {
        let stats = &self.shared.stats;
        self.shared.mark_busy();
        Stats::add(&stats.ticks);
        let started = Instant::now();
        let result = catch_unwind(AssertUnwindSafe(|| app.update()));
        let nanos = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        stats.last_tick_nanos.store(nanos, Ordering::Relaxed);
        if let Err(payload) = result {
            let message = panic_message(payload.as_ref());
            tracing::error!(runtime = %self.shared.id, panic = %message, "bevy update panicked");
            return Flow::Fail(Failure::UpdatePanicked(message));
        }
        stats
            .entities
            .store(u64::from(app.world().entity_count()), Ordering::Relaxed);
        match app.should_exit() {
            None => Flow::Continue,
            Some(AppExit::Success) => Flow::Exit,
            Some(AppExit::Error(code)) => Flow::Fail(Failure::AppExitError(code.get())),
        }
    }

    /// The way down: one last update on a graceful stop, then reject the
    /// queue, close the channel and drop the app on this thread.
    fn finish(self, mut app: Option<App>, flow: Flow) {
        let shared = &self.shared;
        let was = shared.status();
        let graceful = matches!(flow, Flow::Stop) && was == RuntimeStatus::Running;
        let event = match flow {
            Flow::Fail(failure) => {
                tracing::error!(runtime = %shared.id, ?failure, "bevy runtime failed");
                shared.set_failure(failure);
                Event::Fail
            }
            Flow::Continue | Flow::Stop | Flow::Exit => Event::Stop,
        };
        shared.transition(event);
        if graceful && let Some(app) = app.as_mut() {
            app.world_mut().write_message(AppExit::Success);
            if let Flow::Fail(failure) = self.update(app) {
                shared.set_failure(failure);
            }
        }

        let Self { shared, rx } = self;
        while let Ok(msg) = rx.try_recv() {
            shared.stats.dequeue();
            match msg {
                Msg::Run(command) => {
                    Stats::add(&shared.stats.commands_rejected);
                    command.reject(RuntimeError::NotRunning(RuntimeStatus::Stopping));
                }
                Msg::Step { reply, .. } => {
                    Stats::add(&shared.stats.commands_rejected);
                    let _ = reply.send(Err(RuntimeError::NotRunning(RuntimeStatus::Stopping)));
                }
                Msg::Wake => {}
            }
        }
        // A command that comes after the drain drops with the channel. Its
        // caller sees a closed reply and gets `NotRunning`.
        drop(rx);
        shared.stats.queue_depth.store(0, Ordering::Relaxed);

        if let Some(app) = app
            && let Err(payload) = catch_unwind(AssertUnwindSafe(move || drop(app)))
        {
            shared.set_failure(Failure::DropPanicked(panic_message(payload.as_ref())));
        }
        shared.mark_idle();
        let failed = shared.failure().is_some();
        shared.transition(Event::Drained { failed });
        tracing::info!(runtime = %shared.id, status = %shared.status(), "bevy runtime stopped");
    }
}

fn setup_panic(payload: &(dyn std::any::Any + Send)) -> Flow {
    let message = panic_message(payload);
    tracing::error!(panic = %message, "bevy setup panicked");
    Flow::Fail(Failure::SetupPanicked(message))
}

/// Ticks the global Bevy task pools, if the app made them. The plugin never
/// makes them: they are global to the process, so the app decides.
fn tick_task_pools() {
    use bevy_tasks::{AsyncComputeTaskPool, ComputeTaskPool, IoTaskPool};
    if ComputeTaskPool::try_get().is_some()
        && AsyncComputeTaskPool::try_get().is_some()
        && IoTaskPool::try_get().is_some()
    {
        bevy_tasks::tick_global_task_pools_on_main_thread();
    }
}

/// Adds `TimePlugin` when setup did not add it. In manual mode, time then
/// goes forward by a fixed step on each update.
#[cfg(feature = "time")]
fn add_time(app: &mut App, config: &BevyConfig) {
    if app.is_plugin_added::<bevy_time::TimePlugin>() {
        return;
    }
    app.add_plugins(bevy_time::TimePlugin);
    if config.tick_mode == TickMode::Manual {
        app.insert_resource(bevy_time::TimeUpdateStrategy::ManualDuration(
            config.manual_time_step,
        ));
    }
}

#[cfg(not(feature = "time"))]
const fn add_time(_app: &mut App, _config: &BevyConfig) {}
