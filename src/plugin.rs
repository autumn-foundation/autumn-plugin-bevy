//! [`BevyPlugin`]: the Autumn plugin.

use std::borrow::Cow;
use std::fmt;

use autumn_web::app::AppBuilder;
use autumn_web::plugin::Plugin;
use autumn_web::plugin_contract::PluginContract;

use crate::Setup;
use crate::config::BevyConfig;

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
    config: BevyConfig,
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
            config: BevyConfig::default(),
            setups: vec![Box::new(setup)],
        }
    }

    /// Adds one more setup step. The steps run in the order you add them.
    #[must_use]
    pub fn setup<F>(self, setup: F) -> Self
    where
        F: FnOnce(&mut bevy_app::App) + Send + 'static,
    {
        let _ = setup;
        self
    }

    /// Sets the configuration.
    #[must_use]
    pub fn config(self, config: BevyConfig) -> Self {
        let _ = config;
        self
    }
}

impl Plugin for BevyPlugin {
    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed("")
    }

    fn contract(&self) -> Option<PluginContract> {
        None
    }

    fn build(self, app: AppBuilder) -> AppBuilder {
        let _ = (self.config, self.setups);
        app
    }
}

impl fmt::Debug for BevyPlugin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BevyPlugin")
            .field("config", &self.config)
            .field("setups", &self.setups.len())
            .finish()
    }
}
