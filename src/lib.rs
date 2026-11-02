#![doc = include_str!("../README.md")]

pub use bevy_app;
pub use bevy_ecs;
#[cfg(feature = "time")]
pub use bevy_time;

mod command;
mod config;
mod driver;
mod error;
mod handle;
mod health;
mod lifecycle;
mod metrics;
mod plugin;
mod runtime;
mod schedule;
mod shared;
mod status;

pub use config::{BevyConfig, QueueCapacity, TickMode, TickRate};
pub use error::{ConfigError, RuntimeError};
pub use handle::{Bevy, BevyHandle};
pub use health::BevyHealth;
pub use metrics::BevyMetrics;
pub use plugin::BevyPlugin;
pub use runtime::BevyRuntime;
pub use status::{RuntimeId, RuntimeStatus};

/// One setup step. It runs one time, on the Bevy thread, before the first
/// update.
pub type Setup = Box<dyn FnOnce(&mut bevy_app::App) + Send + 'static>;

/// Items for a glob import: `use autumn_plugin_bevy::prelude::*;`.
///
/// This module does not export Bevy `App` or `Plugin`. Their names are the
/// same as Autumn names.
pub mod prelude {
    pub use crate::{
        Bevy, BevyConfig, BevyHandle, BevyPlugin, BevyRuntime, QueueCapacity, RuntimeError,
        RuntimeStatus, TickMode, TickRate,
    };
}
