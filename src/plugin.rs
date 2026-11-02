//! [`BevyPlugin`]: the Autumn plugin.

use std::borrow::Cow;
use std::fmt;

use autumn_web::app::AppBuilder;
use autumn_web::plugin::Plugin;
use autumn_web::plugin_contract::PluginContract;

use std::sync::Arc;

use autumn_web::{AutumnError, AutumnResult};

use crate::Setup;
use crate::config::BevyConfig;
use crate::error::{ConfigError, RuntimeError};
use crate::handle::BevyHandle;
use crate::runtime::BevyRuntime;

/// The `autumn-web` versions that this release supports.
const AUTUMN_WEB_RANGE: &str = "0.9";

/// The Autumn plugin that runs one headless Bevy app.
///
/// ```rust,no_run
/// use autumn_plugin_bevy::BevyPlugin;
///
/// # async fn run() {
/// autumn_web::app()
///     .plugin(BevyPlugin::new(|app| {
///         // Add Bevy systems, resources and plugins here.
///         let _ = app;
///     }))
///     .run()
///     .await;
/// # }
/// ```
///
/// The plugin starts the Bevy thread in a startup hook and stops it in a
/// shutdown hook. It registers the `bevy` health indicator and the `bevy`
/// metrics source. It mounts no routes.
pub struct BevyPlugin {
    config: Option<BevyConfig>,
    setups: Vec<Setup>,
}

impl BevyPlugin {
    /// The plugin name. It is the same as the crate name.
    pub const NAME: &'static str = "autumn-plugin-bevy";

    /// Makes the plugin with one setup step. `setup` runs on the Bevy thread,
    /// after `App::new()`.
    #[must_use]
    pub fn new<F>(setup: F) -> Self
    where
        F: FnOnce(&mut bevy_app::App) + Send + 'static,
    {
        Self {
            config: None,
            setups: vec![Box::new(setup)],
        }
    }

    /// Adds one more setup step. The steps run in the order you add them.
    #[must_use]
    pub fn setup<F>(mut self, setup: F) -> Self
    where
        F: FnOnce(&mut bevy_app::App) + Send + 'static,
    {
        self.setups.push(Box::new(setup));
        self
    }

    /// Sets the configuration in code. Then the plugin does not read the
    /// `[bevy]` table of `autumn.toml`.
    #[must_use]
    pub const fn config(mut self, config: BevyConfig) -> Self {
        self.config = Some(config);
        self
    }
}

impl Plugin for BevyPlugin {
    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed(Self::NAME)
    }

    /// A third-party contract. This crate has its own versions, so it does
    /// not use `lockstep_range`.
    fn contract(&self) -> Option<PluginContract> {
        Some(
            PluginContract::new(env!("CARGO_PKG_NAME"))
                .plugin_version(env!("CARGO_PKG_VERSION"))
                .autumn_web(AUTUMN_WEB_RANGE),
        )
    }

    fn build(self, app: AppBuilder) -> AppBuilder {
        let setups = self.setups;
        let (config, config_error) = resolve_config(self.config, BevyConfig::load_default);
        let runtime = Arc::new(BevyRuntime::new(config, move |bevy: &mut bevy_app::App| {
            for setup in setups {
                setup(bevy);
            }
        }));
        let handle = runtime.handle();
        let health = Arc::new(runtime.health());
        let metrics = Arc::new(runtime.metrics());
        let on_start = Arc::clone(&runtime);
        let on_stop = runtime;
        app.config_section("bevy")
            // Initializers also run in modes that serve no requests (static
            // builds, replays). So this one only publishes the handle. The
            // startup hook starts the thread.
            .state_initializer(move |state| {
                if state.extension::<BevyHandle>().is_some() {
                    tracing::error!("a BevyHandle is already in the app state; keeping the first");
                    return;
                }
                state.insert_extension(handle);
            })
            .on_startup(move |_state| {
                let runtime = Arc::clone(&on_start);
                let config_error = config_error.clone();
                async move {
                    if let Some(err) = config_error {
                        return Err(AutumnError::internal_server_error_msg(format!(
                            "bevy runtime did not start: {err}"
                        )));
                    }
                    start(&runtime).await
                }
            })
            .on_shutdown(move || {
                let runtime = Arc::clone(&on_stop);
                async move { stop(&runtime).await }
            })
            .health_indicator("bevy", health)
            .metrics_source("bevy", metrics)
    }
}

/// The configuration: the one from code, else the `[bevy]` table. A bad table
/// gives the defaults and an error, and the startup hook then stops the boot.
fn resolve_config(
    explicit: Option<BevyConfig>,
    load: impl FnOnce() -> Result<BevyConfig, ConfigError>,
) -> (BevyConfig, Option<ConfigError>) {
    explicit.map_or_else(
        || match load() {
            Ok(config) => (config, None),
            Err(err) => (BevyConfig::default(), Some(err)),
        },
        |config| (config, None),
    )
}

/// The startup hook: start the thread and wait for the first update. An
/// error stops the boot of the server.
async fn start(runtime: &BevyRuntime) -> AutumnResult<()> {
    match runtime.start() {
        Ok(()) | Err(RuntimeError::AlreadyStarted) => {}
        Err(err) => return Err(boot_error(&err)),
    }
    runtime.wait_ready().await.map_err(|err| boot_error(&err))
}

/// The shutdown hook. It logs a late stop and does not return it, because
/// the server stops anyway.
async fn stop(runtime: &BevyRuntime) {
    if let Err(err) = runtime.shutdown().await {
        tracing::warn!(error = %err, "bevy runtime did not stop in time");
    }
}

/// A boot error carries the full text. It goes to the boot log, not to an
/// HTTP client.
fn boot_error(err: &RuntimeError) -> AutumnError {
    AutumnError::internal_server_error_msg(format!("bevy runtime did not start: {err}"))
}

impl fmt::Debug for BevyPlugin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BevyPlugin")
            .field("config", &self.config)
            .field("setups", &self.setups.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::status::RuntimeStatus;

    fn manual() -> BevyConfig {
        BevyConfig::manual().with_shutdown_timeout(Duration::from_secs(5))
    }

    #[test]
    fn config_from_code_wins_and_a_bad_table_is_an_error() {
        let code = BevyConfig::manual();
        let (config, err) = resolve_config(Some(code.clone()), || panic!("must not read the file"));
        assert_eq!(config, code);
        assert!(err.is_none());

        let (config, err) = resolve_config(None, || Ok(BevyConfig::manual()));
        assert_eq!(config, BevyConfig::manual());
        assert!(err.is_none());

        let (config, err) = resolve_config(None, || Err(ConfigError::TickRate(0)));
        assert_eq!(config, BevyConfig::default());
        assert_eq!(err, Some(ConfigError::TickRate(0)));
    }

    #[tokio::test]
    async fn startup_hook_starts_and_is_idempotent() {
        let runtime = BevyRuntime::new(manual(), |_app| {});
        start(&runtime).await.expect("first start");
        assert_eq!(runtime.status(), RuntimeStatus::Running);
        start(&runtime).await.expect("a second call is fine");
        stop(&runtime).await;
    }

    #[tokio::test]
    async fn shutdown_hook_stops_runtime() {
        let runtime = BevyRuntime::new(manual(), |_app| {});
        start(&runtime).await.expect("start");
        stop(&runtime).await;
        assert_eq!(runtime.status(), RuntimeStatus::Stopped);
        // A second stop is a no-op.
        stop(&runtime).await;
    }

    #[tokio::test]
    async fn shutdown_hook_logs_a_late_stop() {
        let config = manual().with_shutdown_timeout(Duration::from_millis(50));
        let runtime = BevyRuntime::new(config, |_app| {});
        start(&runtime).await.expect("start");
        let handle = runtime.handle();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let stuck = tokio::spawn(async move {
            handle
                .with_world_timeout(None, move |_w| {
                    let _ = entered_tx.send(());
                    std::thread::sleep(Duration::from_millis(400));
                })
                .await
        });
        tokio::task::spawn_blocking(move || entered_rx.recv_timeout(Duration::from_secs(10)))
            .await
            .unwrap()
            .expect("the command started");
        stop(&runtime).await;
        assert!(
            !runtime.status().is_terminal(),
            "the hook returns before the thread stops"
        );
        let _ = stuck.await;
    }

    #[tokio::test]
    async fn startup_hook_maps_a_setup_panic() {
        let runtime = BevyRuntime::new(manual(), |_app| panic!("boom in setup"));
        let err = start(&runtime).await.expect_err("setup panics");
        let text = err.to_string();
        assert!(text.contains("bevy runtime did not start"), "{text}");
        assert!(text.contains("boom in setup"), "{text}");
    }

    #[tokio::test]
    async fn startup_hook_refuses_a_stopped_runtime() {
        let runtime = BevyRuntime::new(manual(), |_app| {});
        runtime.shutdown().await.expect("shutdown before start");
        let err = start(&runtime).await.expect_err("stopped");
        assert!(err.to_string().contains("not running"), "{err}");
    }
}
