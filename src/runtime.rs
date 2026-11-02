//! [`BevyRuntime`]: the owner of the Bevy thread.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

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
/// an update, or when your code drops the runtime and all its handles. A live
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
    /// [`RuntimeError::InvalidConfig`] when a value is out of range (see
    /// [`BevyConfig::validate`]), [`RuntimeError::AlreadyStarted`] on a second
    /// call,
    /// [`RuntimeError::NotRunning`] after a shutdown,
    /// [`RuntimeError::Reentrant`] on the Bevy thread, and
    /// [`RuntimeError::Spawn`] when the thread does not start.
    pub fn start(&self) -> Result<(), RuntimeError> {
        if driver::on_runtime_thread(self.shared.id) {
            return Err(RuntimeError::Reentrant);
        }
        self.shared
            .config
            .validate()
            .map_err(RuntimeError::InvalidConfig)?;
        // `take` under the lock: of two calls, only one gets the parts.
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(Pending { setup, rx }) = pending else {
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
    /// inside `TestApp` startup hooks. When the limit passes, it records
    /// `StartupTimeout` and asks the thread to stop. The thread stops at its
    /// next check and then has the status `Failed`. A setup step or a first
    /// update that never returns keeps the thread in `Starting`.
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
        let status = blocking_wait(shared, |s| s != RuntimeStatus::Starting, limit).await;
        // When the boot ends just after the limit, the record fails, and the
        // match below reads the new status.
        if status == RuntimeStatus::Starting && self.shared.record_startup_timeout(limit) {
            // A start that did not end in time must not go on unseen.
            self.wake();
            return Err(RuntimeError::StartupTimeout(limit));
        }
        match self.shared.status() {
            RuntimeStatus::Running => Ok(()),
            other => Err(match self.shared.failure() {
                Some(Failure::SetupPanicked(message)) => RuntimeError::SetupPanicked(message),
                Some(Failure::SetupInvalid(reason)) => RuntimeError::InvalidSetup(reason),
                Some(Failure::StartupTimeout(limit)) => RuntimeError::StartupTimeout(limit),
                _ => RuntimeError::NotRunning(other),
            }),
        }
    }

    /// Stops the runtime and waits for the thread. A call after the thread
    /// stopped is a no-op. After `ShutdownTimeout`, a second call waits again.
    ///
    /// When the status is `Running`, the thread runs one more update with
    /// `AppExit::Success`. Then it rejects the queued commands and drops the
    /// world.
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
        // With no thread, there is nothing to wait for. `start` can run at the
        // same time, so check the result of the transition, not the status.
        if self.shared.status() == RuntimeStatus::NotStarted
            && self.shared.transition(Event::Stop) == Some(RuntimeStatus::Stopped)
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
        let status = blocking_wait(shared, RuntimeStatus::is_terminal, limit).await;
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

/// The length of one wait slice. Between slices, the wait reads the cancel
/// flag.
const WAIT_SLICE: Duration = Duration::from_millis(20);

/// Sets the flag when the async wait goes away (for example, when Autumn's
/// shutdown budget ends and it drops the hook). The blocking wait then ends
/// at its next slice, so it does not hold a thread of the pool.
struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Waits until `done` is true for the status, or until `limit`, off the tokio
/// worker threads. It uses no tokio timer, so it works under `TestApp`. With
/// no tokio runtime, it waits on this thread.
async fn blocking_wait(
    shared: Arc<Shared>,
    done: fn(RuntimeStatus) -> bool,
    limit: Duration,
) -> RuntimeStatus {
    let cancelled = Arc::new(AtomicBool::new(false));
    let wait = {
        let cancelled = Arc::clone(&cancelled);
        move || wait_sliced(&shared, done, limit, &cancelled)
    };
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return wait();
    };
    let _cancel = CancelOnDrop(cancelled);
    let wait = Arc::new(wait);
    let on_pool = Arc::clone(&wait);
    // When the blocking pool shuts down, wait here. The wait has a limit.
    runtime
        .spawn_blocking(move || on_pool())
        .await
        .unwrap_or_else(|_| wait())
}

/// Waits in slices, so that a cancel ends the wait soon.
fn wait_sliced(
    shared: &Shared,
    done: fn(RuntimeStatus) -> bool,
    limit: Duration,
    cancelled: &AtomicBool,
) -> RuntimeStatus {
    let started = Instant::now();
    loop {
        let left = limit.saturating_sub(started.elapsed());
        let status = shared.wait_until(done, left.min(WAIT_SLICE));
        if done(status) || left <= WAIT_SLICE || cancelled.load(Ordering::Acquire) {
            return status;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancelled_wait_ends_at_the_next_slice() {
        let shared = Shared::new(BevyConfig::default());
        let cancelled = AtomicBool::new(true);
        let started = Instant::now();
        let status = wait_sliced(
            &shared,
            RuntimeStatus::is_terminal,
            Duration::from_secs(10),
            &cancelled,
        );
        assert_eq!(status, RuntimeStatus::NotStarted);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_wait_ends_at_its_limit() {
        let shared = Shared::new(BevyConfig::default());
        let cancelled = AtomicBool::new(false);
        let started = Instant::now();
        let _ = wait_sliced(
            &shared,
            RuntimeStatus::is_terminal,
            Duration::from_millis(70),
            &cancelled,
        );
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(60), "{waited:?}");
        assert!(waited < Duration::from_secs(2), "{waited:?}");
    }

    #[test]
    fn a_wait_ends_when_the_status_changes() {
        let shared = Arc::new(Shared::new(BevyConfig::default()));
        let other = Arc::clone(&shared);
        let changer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            other.transition(Event::Stop);
        });
        let cancelled = AtomicBool::new(false);
        let status = wait_sliced(
            &shared,
            RuntimeStatus::is_terminal,
            Duration::from_secs(10),
            &cancelled,
        );
        assert_eq!(status, RuntimeStatus::Stopped);
        changer.join().unwrap();
    }
}
