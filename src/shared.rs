//! State that the Bevy thread and the async side share.
//!
//! Only plain values live here: the status, flags and counters. The `World`
//! never does. Health checks and metrics read this state and never wait on
//! the Bevy thread.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::config::BevyConfig;
use crate::lifecycle::{self, Event};
use crate::status::{RuntimeId, RuntimeStatus};

/// Why the runtime failed. `wait_ready` maps it to an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Setup or the first update panicked. The text is the panic message.
    SetupPanicked(String),
    /// Setup and the first update did not finish in time.
    StartupTimeout(Duration),
    /// An update panicked.
    UpdatePanicked(String),
    /// A system wrote `AppExit::Error`.
    AppExitError(u8),
    /// The drop of the app panicked.
    DropPanicked(String),
    /// Setup did a thing that the plugin does not allow.
    SetupInvalid(&'static str),
}

/// Counters for health and metrics.
#[derive(Debug, Default)]
pub struct Stats {
    pub(crate) ticks: AtomicU64,
    pub(crate) overruns: AtomicU64,
    pub(crate) last_tick_nanos: AtomicU64,
    pub(crate) entities: AtomicU64,
    pub(crate) queue_depth: AtomicUsize,
    pub(crate) commands_ok: AtomicU64,
    pub(crate) commands_panicked: AtomicU64,
    pub(crate) commands_busy: AtomicU64,
    pub(crate) commands_cancelled: AtomicU64,
    pub(crate) commands_timeout: AtomicU64,
    pub(crate) commands_rejected: AtomicU64,
}

impl Stats {
    pub(crate) fn add(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn get(counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }

    /// Adds one to the queue depth. Call it before a send.
    pub(crate) fn enqueue(&self) {
        self.queue_depth.fetch_add(1, Ordering::Relaxed);
    }

    /// Removes one from the queue depth. It never goes below zero.
    pub(crate) fn dequeue(&self) {
        let _ = self
            .queue_depth
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                Some(n.saturating_sub(1))
            });
    }
}

/// The shared state of one runtime.
#[derive(Debug)]
pub struct Shared {
    pub(crate) id: RuntimeId,
    pub(crate) config: BevyConfig,
    pub(crate) stats: Stats,
    status: Mutex<RuntimeStatus>,
    changed: Condvar,
    mirror: AtomicU8,
    stop: AtomicBool,
    failure: Mutex<Option<Failure>>,
    epoch: Instant,
    /// Nanoseconds after `epoch` plus one when the thread started work, or 0
    /// when it waits.
    working_since: AtomicU64,
}

impl Shared {
    pub(crate) fn new(config: BevyConfig) -> Self {
        Self {
            id: RuntimeId::next(),
            config,
            stats: Stats::default(),
            status: Mutex::new(RuntimeStatus::NotStarted),
            changed: Condvar::new(),
            mirror: AtomicU8::new(RuntimeStatus::NotStarted.to_u8()),
            stop: AtomicBool::new(false),
            failure: Mutex::new(None),
            epoch: Instant::now(),
            working_since: AtomicU64::new(0),
        }
    }

    fn lock_status(&self) -> MutexGuard<'_, RuntimeStatus> {
        self.status.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The status, with no lock.
    pub(crate) fn status(&self) -> RuntimeStatus {
        RuntimeStatus::from_u8(self.mirror.load(Ordering::Acquire))
    }

    /// Applies `event` through [`lifecycle::next`]. Returns the new status, or
    /// `None` when the event is not allowed. Then nothing changes.
    pub(crate) fn transition(&self, event: Event) -> Option<RuntimeStatus> {
        let status = self.lock_status();
        self.apply(status, event)
    }

    /// Applies `event` under the held status lock, then notifies the
    /// waiters. The only place that writes the status.
    fn apply(
        &self,
        mut status: MutexGuard<'_, RuntimeStatus>,
        event: Event,
    ) -> Option<RuntimeStatus> {
        let next = lifecycle::next(*status, event);
        match next {
            Some(to) => {
                debug_assert!(lifecycle::rank(to) > lifecycle::rank(*status));
                *status = to;
                self.mirror.store(to.to_u8(), Ordering::Release);
                drop(status);
                self.changed.notify_all();
                tracing::debug!(runtime = %self.id, status = %to, ?event, "bevy runtime status");
            }
            None => {
                tracing::debug!(runtime = %self.id, status = %*status, ?event, "status event refused");
            }
        }
        next
    }

    /// Moves `Starting` to `Running`, but only when no stop and no failure is
    /// recorded. The check and the change happen under the status lock, so
    /// [`Shared::record_startup_timeout`] cannot come between them.
    pub(crate) fn ready_unless_stopped(&self) -> bool {
        let status = self.lock_status();
        if self.stop_requested() || self.failure().is_some() {
            return false;
        }
        self.apply(status, Event::Ready).is_some()
    }

    /// Records a startup timeout and asks the thread to stop, but only while
    /// the status is still `Starting`. Returns `false` when the runtime is
    /// already past `Starting`.
    pub(crate) fn record_startup_timeout(&self, limit: Duration) -> bool {
        let status = self.lock_status();
        if *status != RuntimeStatus::Starting {
            return false;
        }
        self.set_failure(Failure::StartupTimeout(limit));
        self.request_stop();
        drop(status);
        true
    }

    /// Blocks until `done` is `true` for the status, or until `timeout`.
    /// Returns the last status. Do not call it on a tokio worker thread.
    pub(crate) fn wait_until(
        &self,
        done: impl Fn(RuntimeStatus) -> bool,
        timeout: Duration,
    ) -> RuntimeStatus {
        let guard = self.lock_status();
        let (guard, _) = self
            .changed
            .wait_timeout_while(guard, timeout, |status| !done(*status))
            .unwrap_or_else(PoisonError::into_inner);
        *guard
    }

    pub(crate) fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    pub(crate) fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Keeps the first failure. A later one is only logged.
    pub(crate) fn set_failure(&self, failure: Failure) {
        let mut slot = self.failure.lock().unwrap_or_else(PoisonError::into_inner);
        let ignored = if slot.is_none() {
            *slot = Some(failure);
            None
        } else {
            Some(failure)
        };
        drop(slot);
        if let Some(failure) = ignored {
            tracing::debug!(runtime = %self.id, ?failure, "second bevy failure ignored");
        }
    }

    pub(crate) fn failure(&self) -> Option<Failure> {
        self.failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn nanos_since_epoch(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    /// The thread starts work: an update or a command.
    pub(crate) fn mark_working(&self) {
        let now = self.nanos_since_epoch().saturating_add(1);
        self.working_since.store(now, Ordering::Release);
    }

    /// The thread waits for work.
    pub(crate) fn mark_idle(&self) {
        self.working_since.store(0, Ordering::Release);
    }

    /// The time since the thread started its current work. Zero when it
    /// waits.
    pub(crate) fn working_for(&self) -> Duration {
        match self.working_since.load(Ordering::Acquire) {
            0 => Duration::ZERO,
            since => {
                let now = self.nanos_since_epoch().saturating_add(1);
                Duration::from_nanos(now.saturating_sub(since))
            }
        }
    }
}
