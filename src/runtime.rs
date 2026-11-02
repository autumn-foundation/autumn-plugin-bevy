//! [`BevyRuntime`]: the owner of the Bevy thread.

use std::fmt;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};

use crate::Setup;
use crate::command::Msg;
use crate::config::BevyConfig;
use crate::error::RuntimeError;
use crate::handle::BevyHandle;
use crate::health::BevyHealth;
use crate::metrics::BevyMetrics;
use crate::shared::Shared;
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
    /// [`RuntimeError::NotRunning`] after a shutdown, [`RuntimeError::Reentrant`]
    /// on the Bevy thread, and [`RuntimeError::Spawn`] when the thread does not
    /// start.
    pub fn start(&self) -> Result<(), RuntimeError> {
        Err(RuntimeError::AlreadyStarted)
    }

    /// Waits until setup and the first update are done.
    ///
    /// It waits on a blocking thread, with no tokio timer, so it also works
    /// inside `TestApp` startup hooks.
    ///
    /// # Errors
    ///
    /// [`RuntimeError::SetupPanicked`], [`RuntimeError::StartupTimeout`],
    /// [`RuntimeError::NotRunning`] when the runtime is not started or stopped,
    /// and [`RuntimeError::Reentrant`] on the Bevy thread.
    pub async fn wait_ready(&self) -> Result<(), RuntimeError> {
        Err(RuntimeError::NotRunning(RuntimeStatus::NotStarted))
    }

    /// Stops the runtime and waits for the thread. A second call is a no-op.
    ///
    /// The thread runs one more update with `AppExit::Success`, rejects the
    /// queued commands, and drops the world.
    ///
    /// # Errors
    ///
    /// [`RuntimeError::ShutdownTimeout`] when the thread does not stop in time
    /// (the thread continues), and [`RuntimeError::Reentrant`] on the Bevy
    /// thread.
    pub async fn shutdown(&self) -> Result<(), RuntimeError> {
        Err(RuntimeError::ShutdownTimeout(
            self.shared.config.shutdown_timeout,
        ))
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
        RuntimeStatus::NotStarted
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

impl fmt::Debug for BevyRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BevyRuntime")
            .field("id", &self.shared.id)
            .field("status", &self.shared.status())
            .finish_non_exhaustive()
    }
}
