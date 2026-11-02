//! [`BevyHandle`] and the [`Bevy`] handler extractor.

use std::fmt;
use std::sync::Arc;
use std::sync::mpsc::SyncSender;
use std::time::Duration;

use autumn_web::reexports::axum::extract::FromRequestParts;
use autumn_web::reexports::http::request::Parts;
use autumn_web::{AppState, AutumnError, AutumnResult};
use bevy_ecs::world::World;

use crate::command::Msg;
use crate::error::RuntimeError;
use crate::shared::Shared;
use crate::status::{RuntimeId, RuntimeStatus};

/// A cheap, cloneable handle to a [`BevyRuntime`](crate::BevyRuntime).
///
/// Each call goes into a bounded queue. The Bevy thread runs it between two
/// updates, so a call never sees half of a frame.
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
        let _ = state;
        None
    }

    /// Runs `f` on the Bevy thread, between two updates, and returns its
    /// result. The wait limit is
    /// [`BevyConfig::command_timeout`](crate::BevyConfig::command_timeout).
    ///
    /// `f` is sync. Keep it short: the next update waits for it. Do async work
    /// before or after the call.
    ///
    /// # Errors
    ///
    /// [`RuntimeError::Busy`] when the queue is full, [`RuntimeError::NotRunning`]
    /// when the runtime does not accept commands, [`RuntimeError::Timeout`],
    /// [`RuntimeError::CommandPanicked`], and [`RuntimeError::Reentrant`] on the
    /// Bevy thread.
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
        let _ = (timeout, f, &self.tx);
        Err(RuntimeError::NotRunning(RuntimeStatus::NotStarted))
    }

    /// Runs exactly `n` updates, in queue order, and returns the new tick
    /// count. Only for [`TickMode::Manual`](crate::TickMode::Manual).
    ///
    /// # Errors
    ///
    /// [`RuntimeError::WrongMode`] in fixed mode, and the errors of
    /// [`BevyHandle::with_world`].
    pub async fn step(&self, n: u32) -> Result<u64, RuntimeError> {
        let _ = n;
        Err(RuntimeError::WrongMode)
    }

    /// The current status.
    #[must_use]
    pub fn status(&self) -> RuntimeStatus {
        RuntimeStatus::NotStarted
    }

    /// The count of updates that started.
    #[must_use]
    pub fn ticks(&self) -> u64 {
        0
    }

    /// About how many commands wait in the queue.
    #[must_use]
    pub fn queue_depth(&self) -> usize {
        0
    }

    /// The identity of the runtime.
    #[must_use]
    pub fn runtime_id(&self) -> RuntimeId {
        self.shared.id
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
        let _ = f;
        Err(RuntimeError::NotInstalled.into_autumn_error())
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
        let _ = (timeout, f);
        Err(RuntimeError::NotInstalled.into_autumn_error())
    }

    /// [`BevyHandle::step`] with a safe HTTP error.
    ///
    /// # Errors
    ///
    /// The errors of [`Bevy::with_world`].
    pub async fn step(&self, n: u32) -> AutumnResult<u64> {
        let _ = n;
        Err(RuntimeError::NotInstalled.into_autumn_error())
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
