//! Plugin configuration: code first, with an optional `[bevy]` table.

use std::num::{NonZeroU32, NonZeroUsize};
use std::path::Path;
use std::time::Duration;

use crate::error::ConfigError;

/// Updates per second in [`TickMode::Fixed`]. The range is `1..=1000`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TickRate(NonZeroU32);

impl TickRate {
    /// The highest rate.
    pub const MAX_HZ: u32 = 1_000;
    /// 60 updates per second.
    pub const DEFAULT: Self = Self(nonzero_u32(60));

    /// Makes a rate of `hz` updates per second.
    ///
    /// # Errors
    ///
    /// [`ConfigError::TickRate`] when `hz` is 0 or more than [`Self::MAX_HZ`].
    pub fn new(hz: u32) -> Result<Self, ConfigError> {
        let _ = hz;
        Err(ConfigError::TickRate(hz))
    }

    /// Updates per second.
    #[must_use]
    pub const fn hz(self) -> u32 {
        self.0.get()
    }

    /// The time between two updates.
    #[must_use]
    pub const fn period(self) -> Duration {
        Duration::ZERO
    }
}

impl Default for TickRate {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The most commands that can wait in the queue. The range is `1..=65536`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct QueueCapacity(NonZeroUsize);

impl QueueCapacity {
    /// The largest capacity.
    pub const MAX: usize = 65_536;
    /// 1024 commands.
    pub const DEFAULT: Self = Self(nonzero_usize(1_024));

    /// Makes a capacity of `n` commands.
    ///
    /// # Errors
    ///
    /// [`ConfigError::QueueCapacity`] when `n` is 0 or more than [`Self::MAX`].
    pub fn new(n: usize) -> Result<Self, ConfigError> {
        Err(ConfigError::QueueCapacity(n))
    }

    /// The capacity.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0.get()
    }
}

impl Default for QueueCapacity {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// How the runtime starts an update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TickMode {
    /// Updates run at a fixed rate. A late update skips the missed ticks.
    Fixed(TickRate),
    /// Updates run only for [`BevyHandle::step`](crate::BevyHandle::step).
    /// Use it for tests and turn-based apps.
    Manual,
}

impl Default for TickMode {
    fn default() -> Self {
        Self::Fixed(TickRate::DEFAULT)
    }
}

/// The configuration of the Bevy runtime.
///
/// The defaults are safe for a server. Change them with the `with_*` methods,
/// or read the `[bevy]` table of `autumn.toml` with
/// [`BevyConfig::from_toml_str`] or [`BevyConfig::load`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BevyConfig {
    /// How the runtime starts an update. Default: 60 updates per second.
    pub tick_mode: TickMode,
    /// The most commands that can wait. Default: 1024.
    pub queue_capacity: QueueCapacity,
    /// The time a caller waits for a command. `None` waits with no limit.
    /// Default: 5 s.
    pub command_timeout: Option<Duration>,
    /// The time the server waits for setup and the first update. Default:
    /// 10 s.
    pub startup_timeout: Duration,
    /// The health check is down when the thread is busy for longer than
    /// this. Default: 1 s.
    pub stall_threshold: Duration,
    /// The time shutdown waits for the thread to stop. Default: 5 s.
    pub shutdown_timeout: Duration,
    /// The step of `Time` on each update in [`TickMode::Manual`]. It applies
    /// only with the `time` feature, when the plugin adds `TimePlugin`.
    /// Default: 1/60 s.
    pub manual_time_step: Duration,
}

impl Default for BevyConfig {
    fn default() -> Self {
        Self {
            tick_mode: TickMode::default(),
            queue_capacity: QueueCapacity::DEFAULT,
            command_timeout: Some(Duration::from_secs(5)),
            startup_timeout: Duration::from_secs(10),
            stall_threshold: Duration::from_secs(1),
            shutdown_timeout: Duration::from_secs(5),
            manual_time_step: Duration::from_nanos(1_000_000_000 / 60),
        }
    }
}

impl BevyConfig {
    /// The defaults with [`TickMode::Manual`].
    #[must_use]
    pub fn manual() -> Self {
        Self::default()
    }

    /// Reads the `[bevy]` table of a full `autumn.toml` text. With no table,
    /// it gives the defaults.
    ///
    /// # Errors
    ///
    /// [`ConfigError`] for a TOML error, an unknown key, or a value out of
    /// range. The error names the key.
    pub fn from_toml_str(autumn_toml: &str) -> Result<Self, ConfigError> {
        let _ = autumn_toml;
        Err(ConfigError::Parse(String::new()))
    }

    /// Reads the `[bevy]` table of the `autumn.toml` file at `path`. A
    /// missing file gives the defaults.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Read`] when the file exists but cannot be read, and the
    /// errors of [`BevyConfig::from_toml_str`].
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let _ = path.as_ref();
        Err(ConfigError::Parse(String::new()))
    }

    /// Sets [`BevyConfig::tick_mode`].
    #[must_use]
    pub const fn with_tick_mode(self, mode: TickMode) -> Self {
        let _ = mode;
        self
    }

    /// Sets [`BevyConfig::queue_capacity`].
    #[must_use]
    pub const fn with_queue_capacity(self, capacity: QueueCapacity) -> Self {
        let _ = capacity;
        self
    }

    /// Sets [`BevyConfig::command_timeout`]. `None` waits with no limit.
    #[must_use]
    pub const fn with_command_timeout(self, timeout: Option<Duration>) -> Self {
        let _ = timeout;
        self
    }

    /// Sets [`BevyConfig::startup_timeout`].
    #[must_use]
    pub const fn with_startup_timeout(self, timeout: Duration) -> Self {
        let _ = timeout;
        self
    }

    /// Sets [`BevyConfig::stall_threshold`].
    #[must_use]
    pub const fn with_stall_threshold(self, threshold: Duration) -> Self {
        let _ = threshold;
        self
    }

    /// Sets [`BevyConfig::shutdown_timeout`].
    #[must_use]
    pub const fn with_shutdown_timeout(self, timeout: Duration) -> Self {
        let _ = timeout;
        self
    }

    /// Sets [`BevyConfig::manual_time_step`].
    #[must_use]
    pub const fn with_manual_time_step(self, step: Duration) -> Self {
        let _ = step;
        self
    }
}

const fn nonzero_u32(value: u32) -> NonZeroU32 {
    match NonZeroU32::new(value) {
        Some(n) => n,
        None => NonZeroU32::MIN,
    }
}

const fn nonzero_usize(value: usize) -> NonZeroUsize {
    match NonZeroUsize::new(value) {
        Some(n) => n,
        None => NonZeroUsize::MIN,
    }
}
