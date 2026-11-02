//! The loop on the `autumn-bevy` thread. Only this thread touches the
//! `App` and the `World`.
//!
//! Order in each pass: the stop flag, then a due update, then one message.
//! So a flood of commands cannot stop the updates (invariant I9).

use std::cell::Cell;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use bevy_app::{App, AppExit, MainSchedulePlugin, PluginsState};

use crate::Setup;
use crate::command::{Msg, contain_drop, panic_message};
use crate::config::{BevyConfig, TickMode};
use crate::error::RuntimeError;
use crate::lifecycle::Event;
use crate::schedule::next_deadline;
use crate::shared::{Failure, Shared, Stats};
use crate::status::{RuntimeId, RuntimeStatus};

/// The name of the Bevy thread.
pub const THREAD_NAME: &str = "autumn-bevy";

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
pub fn on_runtime_thread(id: RuntimeId) -> bool {
    CURRENT.with(|current| current.get() == Some(id))
}

/// Starts the thread. The thread owns `rx` and the app until it ends.
pub fn spawn(shared: Arc<Shared>, setup: Setup, rx: Receiver<Msg>) -> io::Result<()> {
    std::thread::Builder::new()
        .name(THREAD_NAME.to_owned())
        .spawn(move || {
            let _guard = UnwindGuard(Arc::clone(&shared));
            Driver { shared, rx }.run(setup);
        })
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

const fn is_continue(flow: &Flow) -> bool {
    matches!(flow, Flow::Continue)
}

/// The last line of defence. A panic that the loop did not contain ends the
/// thread. The guard then moves the status to `Failed`, so callers and the
/// health check do not see a live runtime that is gone.
struct UnwindGuard(Arc<Shared>);

impl Drop for UnwindGuard {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            return;
        }
        let shared = &self.0;
        tracing::error!(runtime = %shared.id, "the bevy thread ended with a panic");
        shared.set_failure(Failure::UpdatePanicked(
            "the bevy thread ended with a panic".to_owned(),
        ));
        shared.transition(Event::Fail);
        shared.stats.queue_depth.store(0, Ordering::Relaxed);
        shared.mark_idle();
        shared.transition(Event::Drained { failed: true });
    }
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
            // A stop or a failure (a startup timeout) can come while the boot
            // runs. Then the runtime never goes to `Running`.
            (Some(app), Flow::Continue) if self.stop_or_failure() => (Some(app), Flow::Stop),
            (Some(mut app), Flow::Continue) => {
                if self.shared.ready_unless_stopped() {
                    tracing::info!(runtime = %self.shared.id, "bevy runtime is running");
                    let flow = self.drive(&mut app);
                    (Some(app), flow)
                } else {
                    (Some(app), Flow::Stop)
                }
            }
            other => other,
        };
        self.finish(app, flow);
    }

    fn stop_or_failure(&self) -> bool {
        self.shared.stop_requested() || self.shared.failure().is_some()
    }

    /// Setup, the plugin build steps, and the first update.
    fn boot(&self, setup: Setup) -> (Option<App>, Flow) {
        self.shared.mark_working();
        let config = &self.shared.config;
        // `start` checked the limit, so the addition cannot overflow. Keep the
        // check anyway: no limit is better than a panic.
        let deadline = Instant::now().checked_add(config.startup_timeout);
        let built = catch_unwind(AssertUnwindSafe(|| {
            let mut app = App::new();
            setup(&mut app);
            // `App::run` leaves `App::empty()` behind: an app with no main
            // schedule. A setup that puts a new `App::new()` in place is fine.
            let drivable = app.is_plugin_added::<MainSchedulePlugin>();
            if drivable {
                add_time(&mut app, config);
            }
            (app, drivable)
        }));
        let mut app = match built {
            Ok((app, true)) => app,
            Ok((app, false)) => {
                return (
                    Some(app),
                    Flow::Fail(Failure::SetupInvalid(
                        "setup must not call App::run; the plugin drives the app",
                    )),
                );
            }
            Err(payload) => return (None, setup_panic(payload)),
        };
        loop {
            if self.shared.stop_requested() {
                return (Some(app), Flow::Stop);
            }
            match catch_unwind(AssertUnwindSafe(|| app.plugins_state())) {
                Ok(PluginsState::Adding) => {}
                Ok(PluginsState::Cleaned) => break,
                Ok(PluginsState::Ready | PluginsState::Finished) => {
                    let done = catch_unwind(AssertUnwindSafe(|| {
                        app.finish();
                        app.cleanup();
                    }));
                    if let Err(payload) = done {
                        return (Some(app), setup_panic(payload));
                    }
                    break;
                }
                Err(payload) => return (Some(app), setup_panic(payload)),
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return (
                    Some(app),
                    Flow::Fail(Failure::StartupTimeout(config.startup_timeout)),
                );
            }
            tick_task_pools();
            std::thread::sleep(PLUGIN_POLL);
        }
        if self.stop_or_failure() {
            return (Some(app), Flow::Stop);
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
                    self.shared.mark_working();
                    let flow = self.update(app);
                    if !is_continue(&flow) {
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
                    self.shared.mark_working();
                    let flow = self.handle(app, msg);
                    if !is_continue(&flow) {
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
                // One step is one piece of work, so the stall rule sees all
                // of it. Before each update, read the stop flag and the caller.
                // Outcomes: `cancelled` when no update ran, `ok` when at least
                // one ran, `rejected` when a stop or a failure ended it.
                for done in 0..n {
                    if reply.is_closed() {
                        let counter = if done == 0 {
                            &stats.commands_cancelled
                        } else {
                            &stats.commands_ok
                        };
                        Stats::add(counter);
                        return Flow::Continue;
                    }
                    if self.shared.stop_requested() {
                        Stats::add(&stats.commands_rejected);
                        let _ = reply.send(Err(RuntimeError::NotRunning(RuntimeStatus::Stopping)));
                        return Flow::Stop;
                    }
                    let flow = self.update(app);
                    if !is_continue(&flow) {
                        Stats::add(&stats.commands_rejected);
                        let _ = reply.send(Err(RuntimeError::NotRunning(RuntimeStatus::Stopping)));
                        return flow;
                    }
                }
                Stats::add(&stats.commands_ok);
                let _ = reply.send(Ok(Stats::get(&stats.ticks)));
                Flow::Continue
            }
            Msg::Wake => Flow::Continue,
        }
    }

    /// One `App::update`, with its panic contained. The caller marks the
    /// thread as working.
    fn update(&self, app: &mut App) -> Flow {
        let stats = &self.shared.stats;
        Stats::add(&stats.ticks);
        let started = Instant::now();
        let result = catch_unwind(AssertUnwindSafe(|| app.update()));
        let nanos = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        stats.last_tick_nanos.store(nanos, Ordering::Relaxed);
        if let Err(payload) = result {
            let message = panic_message(payload.as_ref());
            contain_drop(payload);
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
        let stop_flow = matches!(flow, Flow::Stop);
        if let Flow::Fail(failure) = flow {
            tracing::error!(runtime = %shared.id, ?failure, "bevy runtime failed");
            shared.set_failure(failure);
        }
        // A failure that another thread recorded (a startup timeout) also
        // makes this a `Fail`. Then the last update does not run (I10).
        let failed = shared.failure().is_some();
        shared.transition(if failed { Event::Fail } else { Event::Stop });
        let graceful = stop_flow && was == RuntimeStatus::Running && !failed;
        if graceful && let Some(app) = app.as_mut() {
            shared.mark_working();
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
        // caller sees a closed reply and gets `NotRunning`. A closure can
        // panic in its drop, so drop the channel inside a guard.
        if catch_unwind(AssertUnwindSafe(move || drop(rx))).is_err() {
            tracing::error!(runtime = %shared.id, "a queued closure panicked in its drop");
        }
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

fn setup_panic(payload: Box<dyn std::any::Any + Send>) -> Flow {
    let message = panic_message(payload.as_ref());
    contain_drop(payload);
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
/// goes forward by a fixed step on each update, unless setup set its own
/// `TimeUpdateStrategy`.
#[cfg(feature = "time")]
fn add_time(app: &mut App, config: &BevyConfig) {
    if app.is_plugin_added::<bevy_time::TimePlugin>() {
        return;
    }
    let own_strategy = app
        .world()
        .contains_resource::<bevy_time::TimeUpdateStrategy>();
    app.add_plugins(bevy_time::TimePlugin);
    if config.tick_mode == TickMode::Manual && !own_strategy {
        app.insert_resource(bevy_time::TimeUpdateStrategy::ManualDuration(
            config.manual_time_step,
        ));
    }
}

#[cfg(not(feature = "time"))]
const fn add_time(_app: &mut App, _config: &BevyConfig) {}
