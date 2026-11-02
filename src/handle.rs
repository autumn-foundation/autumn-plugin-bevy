//! [`BevyHandle`] and the [`Bevy`] handler extractor.

use std::fmt;
use std::sync::Arc;
use std::sync::mpsc::{SyncSender, TrySendError};
use std::time::Duration;

use autumn_web::reexports::axum::extract::FromRequestParts;
use autumn_web::reexports::http::request::Parts;
use autumn_web::{AppState, AutumnError, AutumnResult};
use bevy_ecs::world::World;
use tokio::sync::oneshot;

use crate::command::{Job, Msg};
use crate::config::TickMode;
use crate::driver;
use crate::error::RuntimeError;
use crate::shared::{Shared, Stats};
use crate::status::{RuntimeId, RuntimeStatus};

/// The reply side of one queued message.
type Reply<T> = oneshot::Receiver<Result<T, RuntimeError>>;

/// A cheap, cloneable handle to a [`BevyRuntime`](crate::BevyRuntime).
///
/// Each call goes into a bounded queue. The Bevy thread runs it between two
/// updates, so a call never sees half of a frame. A live handle keeps the
/// Bevy thread running.
#[derive(Clone)]
pub struct BevyHandle {
    tx: SyncSender<Msg>,
    shared: Arc<Shared>,
}

impl BevyHandle {
    pub(crate) const fn new(tx: SyncSender<Msg>, shared: Arc<Shared>) -> Self {
        Self { tx, shared }
    }

    /// The handle that [`BevyPlugin`](crate::BevyPlugin) put into the app
    /// state, if any.
    #[must_use]
    pub fn from_state(state: &AppState) -> Option<Self> {
        state.extension::<Self>().map(|handle| (*handle).clone())
    }

    /// Runs `f` on the Bevy thread, between two updates, and returns its
    /// result. The wait limit is
    /// [`BevyConfig::command_timeout`](crate::BevyConfig::command_timeout).
    ///
    /// `f` is sync. Keep it short: the next update waits for it. Do async work
    /// before or after the call. When the wait limit passes or the caller
    /// goes away before `f` starts, `f` does not run.
    ///
    /// # Errors
    ///
    /// [`RuntimeError::Busy`] when the queue is full,
    /// [`RuntimeError::NotRunning`] when the runtime does not accept commands,
    /// [`RuntimeError::Timeout`], [`RuntimeError::CommandPanicked`], and
    /// [`RuntimeError::Reentrant`] on the Bevy thread.
    pub async fn with_world<F, R>(&self, f: F) -> Result<R, RuntimeError>
    where
        F: FnOnce(&mut World) -> R + Send + 'static,
        R: Send + 'static,
    {
        self.with_world_timeout(self.shared.config.command_timeout, f)
            .await
    }

    /// The same as [`BevyHandle::with_world`], with its own wait limit.
    /// `None` waits with no limit. A `Some` limit needs a tokio runtime with
    /// the time driver.
    ///
    /// # Errors
    ///
    /// The errors of [`BevyHandle::with_world`].
    pub async fn with_world_timeout<F, R>(
        &self,
        timeout: Option<Duration>,
        f: F,
    ) -> Result<R, RuntimeError>
    where
        F: FnOnce(&mut World) -> R + Send + 'static,
        R: Send + 'static,
    {
        let reply = self.submit(|reply| Msg::Run(Box::new(Job { f, reply })))?;
        self.wait(reply, timeout).await
    }

    /// Runs exactly `n` updates, in queue order, and returns the new tick
    /// count. Only for [`TickMode::Manual`].
    ///
    /// # Errors
    ///
    /// [`RuntimeError::WrongMode`] in fixed mode, and the errors of
    /// [`BevyHandle::with_world`].
    pub async fn step(&self, n: u32) -> Result<u64, RuntimeError> {
        if driver::on_runtime_thread(self.shared.id) {
            return Err(RuntimeError::Reentrant);
        }
        if self.shared.config.tick_mode != TickMode::Manual {
            return Err(RuntimeError::WrongMode);
        }
        let reply = self.submit(|reply| Msg::Step { n, reply })?;
        self.wait(reply, self.shared.config.command_timeout).await
    }

    /// The current status.
    #[must_use]
    pub fn status(&self) -> RuntimeStatus {
        self.shared.status()
    }

    /// The count of updates that started.
    #[must_use]
    pub fn ticks(&self) -> u64 {
        Stats::get(&self.shared.stats.ticks)
    }

    /// About how many messages wait in the queue.
    #[must_use]
    pub fn queue_depth(&self) -> usize {
        self.shared
            .stats
            .queue_depth
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The identity of the runtime.
    #[must_use]
    pub fn runtime_id(&self) -> RuntimeId {
        self.shared.id
    }

    /// Puts one message into the queue. It never blocks.
    fn submit<T>(
        &self,
        message: impl FnOnce(oneshot::Sender<Result<T, RuntimeError>>) -> Msg,
    ) -> Result<Reply<T>, RuntimeError> {
        if driver::on_runtime_thread(self.shared.id) {
            return Err(RuntimeError::Reentrant);
        }
        let status = self.shared.status();
        if !status.accepts_commands() {
            return Err(RuntimeError::NotRunning(status));
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        let counters = &self.shared.stats;
        counters.enqueue();
        match self.tx.try_send(message(reply_tx)) {
            Ok(()) => Ok(reply_rx),
            Err(TrySendError::Full(_)) => {
                counters.dequeue();
                Stats::add(&counters.commands_busy);
                Err(RuntimeError::Busy)
            }
            Err(TrySendError::Disconnected(_)) => {
                counters.dequeue();
                Err(RuntimeError::NotRunning(self.shared.status()))
            }
        }
    }

    /// Waits for a reply. A closed reply means that the thread has gone.
    async fn wait<T>(&self, reply: Reply<T>, timeout: Option<Duration>) -> Result<T, RuntimeError> {
        let received = match timeout {
            Some(limit) => {
                if let Ok(received) = tokio::time::timeout(limit, reply).await {
                    received
                } else {
                    Stats::add(&self.shared.stats.commands_timeout);
                    return Err(RuntimeError::Timeout(limit));
                }
            }
            None => reply.await,
        };
        received.unwrap_or_else(|_| Err(RuntimeError::NotRunning(self.shared.status())))
    }
}

impl fmt::Debug for BevyHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BevyHandle")
            .field("runtime", &self.shared.id)
            .field("status", &self.shared.status())
            .finish_non_exhaustive()
    }
}

/// The handler extractor for the Bevy runtime of the app.
///
/// ```rust,no_run
/// use autumn_plugin_bevy::Bevy;
/// use autumn_web::AutumnResult;
///
/// async fn count(bevy: Bevy) -> AutumnResult<String> {
///     let n = bevy.with_world(|world| world.entity_count()).await?;
///     Ok(n.to_string())
/// }
/// ```
///
/// Its methods return [`AutumnResult`] with a safe HTTP error: 503 when a
/// retry can help, 500 else, and a fixed body. Use [`Bevy::handle`] to get
/// the typed [`RuntimeError`].
///
/// When the app has no [`BevyPlugin`](crate::BevyPlugin), the extractor
/// fails with 500 and the body "internal error".
#[derive(Clone, Debug)]
pub struct Bevy(BevyHandle);

impl Bevy {
    /// The typed handle.
    #[must_use]
    pub const fn handle(&self) -> &BevyHandle {
        &self.0
    }

    /// The typed handle.
    #[must_use]
    pub fn into_inner(self) -> BevyHandle {
        self.0
    }

    /// [`BevyHandle::with_world`] with a safe HTTP error.
    ///
    /// # Errors
    ///
    /// 503 for `Busy`, `NotRunning` and `Timeout`. 500 for the other errors.
    pub async fn with_world<F, R>(&self, f: F) -> AutumnResult<R>
    where
        F: FnOnce(&mut World) -> R + Send + 'static,
        R: Send + 'static,
    {
        self.0
            .with_world(f)
            .await
            .map_err(RuntimeError::into_autumn_error)
    }

    /// [`BevyHandle::with_world_timeout`] with a safe HTTP error.
    ///
    /// # Errors
    ///
    /// The errors of [`Bevy::with_world`].
    pub async fn with_world_timeout<F, R>(&self, timeout: Option<Duration>, f: F) -> AutumnResult<R>
    where
        F: FnOnce(&mut World) -> R + Send + 'static,
        R: Send + 'static,
    {
        self.0
            .with_world_timeout(timeout, f)
            .await
            .map_err(RuntimeError::into_autumn_error)
    }

    /// [`BevyHandle::step`] with a safe HTTP error.
    ///
    /// # Errors
    ///
    /// The errors of [`Bevy::with_world`].
    pub async fn step(&self, n: u32) -> AutumnResult<u64> {
        self.0
            .step(n)
            .await
            .map_err(RuntimeError::into_autumn_error)
    }

    /// The current status.
    #[must_use]
    pub fn status(&self) -> RuntimeStatus {
        self.0.status()
    }

    /// The count of updates that started.
    #[must_use]
    pub fn ticks(&self) -> u64 {
        self.0.ticks()
    }
}

impl FromRequestParts<AppState> for Bevy {
    type Rejection = AutumnError;

    async fn from_request_parts(
        _parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        BevyHandle::from_state(state)
            .map(Self)
            .ok_or_else(|| RuntimeError::NotInstalled.into_autumn_error())
    }
}
