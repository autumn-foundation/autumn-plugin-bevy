//! Plugin configuration: code first, with an optional `[bevy]` table.

use std::num::{NonZeroU32, NonZeroUsize};
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

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
        if hz > Self::MAX_HZ {
            return Err(ConfigError::TickRate(hz));
        }
        NonZeroU32::new(hz)
            .map(Self)
            .ok_or(ConfigError::TickRate(hz))
    }

    /// Updates per second.
    #[must_use]
    pub const fn hz(self) -> u32 {
        self.0.get()
    }

    /// The time between two updates.
    #[must_use]
    pub fn period(self) -> Duration {
        Duration::from_nanos(1_000_000_000 / u64::from(self.0.get()))
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
        if n > Self::MAX {
            return Err(ConfigError::QueueCapacity(n));
        }
        NonZeroUsize::new(n)
            .map(Self)
            .ok_or(ConfigError::QueueCapacity(n))
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
    /// The health check is down when the thread works on one item for longer than
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
        Self {
            tick_mode: TickMode::Manual,
            ..Self::default()
        }
    }

    /// Reads the `[bevy]` table of a full `autumn.toml` text. With no table,
    /// it gives the defaults.
    ///
    /// # Errors
    ///
    /// [`ConfigError`] for a TOML error, an unknown key, or a value out of
    /// range. The error names the key.
    pub fn from_toml_str(autumn_toml: &str) -> Result<Self, ConfigError> {
        // Parse the whole text. Then a TOML error shows the source line,
        // and so it names the key.
        let document: Document =
            toml::from_str(autumn_toml).map_err(|e| ConfigError::Parse(e.to_string()))?;
        document
            .bevy
            .map_or_else(|| Ok(Self::default()), Section::into_config)
    }

    /// Reads the `[bevy]` table of the `autumn.toml` file at `path`. A
    /// missing file gives the defaults.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Read`] when the file exists but cannot be read, and the
    /// errors of [`BevyConfig::from_toml_str`].
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_toml_str(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(ConfigError::Read {
                path: path.display().to_string(),
                message: e.to_string(),
            }),
        }
    }

    /// Checks each value.
    ///
    /// # Errors
    ///
    /// The first value out of range.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if let Some(timeout) = self.command_timeout {
            check_duration("command_timeout_ms", timeout)?;
        }
        check_duration("startup_timeout_ms", self.startup_timeout)?;
        check_duration("stall_threshold_ms", self.stall_threshold)?;
        check_duration("shutdown_timeout_ms", self.shutdown_timeout)?;
        check_duration("manual_time_step_ms", self.manual_time_step)
    }

    /// Reads the `[bevy]` table of the `autumn.toml` that Autumn reads: in
    /// `AUTUMN_MANIFEST_DIR` first, then in the current directory. A missing
    /// file gives the defaults. Profile files and `AUTUMN_*` variables do not
    /// apply to this table.
    ///
    /// [`BevyPlugin`](crate::BevyPlugin) calls it when the app gives no
    /// configuration in code.
    ///
    /// # Errors
    ///
    /// The errors of [`BevyConfig::load`].
    pub fn load_default() -> Result<Self, ConfigError> {
        Self::load(default_path(&autumn_web::config::OsEnv))
    }

    /// Sets [`BevyConfig::tick_mode`].
    #[must_use]
    pub const fn with_tick_mode(mut self, mode: TickMode) -> Self {
        self.tick_mode = mode;
        self
    }

    /// Sets [`BevyConfig::queue_capacity`].
    #[must_use]
    pub const fn with_queue_capacity(mut self, capacity: QueueCapacity) -> Self {
        self.queue_capacity = capacity;
        self
    }

    /// Sets [`BevyConfig::command_timeout`]. `None` waits with no limit.
    #[must_use]
    pub const fn with_command_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.command_timeout = timeout;
        self
    }

    /// Sets [`BevyConfig::startup_timeout`].
    #[must_use]
    pub const fn with_startup_timeout(mut self, timeout: Duration) -> Self {
        self.startup_timeout = timeout;
        self
    }

    /// Sets [`BevyConfig::stall_threshold`].
    #[must_use]
    pub const fn with_stall_threshold(mut self, threshold: Duration) -> Self {
        self.stall_threshold = threshold;
        self
    }

    /// Sets [`BevyConfig::shutdown_timeout`].
    #[must_use]
    pub const fn with_shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.shutdown_timeout = timeout;
        self
    }

    /// Sets [`BevyConfig::manual_time_step`].
    #[must_use]
    pub const fn with_manual_time_step(mut self, step: Duration) -> Self {
        self.manual_time_step = step;
        self
    }
}

/// The `autumn.toml` path, found the same way as Autumn finds it.
fn default_path(env: &dyn autumn_web::config::Env) -> std::path::PathBuf {
    const FILE: &str = "autumn.toml";
    if let Ok(dir) = env.var("AUTUMN_MANIFEST_DIR") {
        let candidate = Path::new(&dir).join(FILE);
        if candidate.exists() {
            return candidate;
        }
    }
    std::path::PathBuf::from(FILE)
}

/// An `autumn.toml` document, seen only through its `[bevy]` table.
#[derive(Debug, Deserialize)]
struct Document {
    bevy: Option<Section>,
}

/// The `[bevy]` table. Each key is optional. An unknown key is an error, so
/// a typo does not go unseen.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Section {
    tick_mode: Option<ModeName>,
    tick_hz: Option<u32>,
    queue_capacity: Option<usize>,
    command_timeout_ms: Option<u64>,
    startup_timeout_ms: Option<u64>,
    stall_threshold_ms: Option<u64>,
    shutdown_timeout_ms: Option<u64>,
    manual_time_step_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ModeName {
    Fixed,
    Manual,
}

impl Section {
    fn into_config(self) -> Result<BevyConfig, ConfigError> {
        let defaults = BevyConfig::default();
        let rate = self.tick_hz.map(TickRate::new).transpose()?;
        let tick_mode = match self.tick_mode.unwrap_or(ModeName::Fixed) {
            ModeName::Fixed => TickMode::Fixed(rate.unwrap_or_default()),
            ModeName::Manual => TickMode::Manual,
        };
        let queue_capacity = self
            .queue_capacity
            .map(QueueCapacity::new)
            .transpose()?
            .unwrap_or_default();
        let command_timeout =
            millis("command_timeout_ms", self.command_timeout_ms)?.or(defaults.command_timeout);
        Ok(BevyConfig {
            tick_mode,
            queue_capacity,
            command_timeout,
            startup_timeout: millis("startup_timeout_ms", self.startup_timeout_ms)?
                .unwrap_or(defaults.startup_timeout),
            stall_threshold: millis("stall_threshold_ms", self.stall_threshold_ms)?
                .unwrap_or(defaults.stall_threshold),
            shutdown_timeout: millis("shutdown_timeout_ms", self.shutdown_timeout_ms)?
                .unwrap_or(defaults.shutdown_timeout),
            manual_time_step: millis("manual_time_step_ms", self.manual_time_step_ms)?
                .unwrap_or(defaults.manual_time_step),
        })
    }
}

/// The longest duration that a key accepts: 24 hours. A longer value is
/// almost certainly a mistake, and it can overflow `Instant` arithmetic.
pub const MAX_DURATION: Duration = Duration::from_hours(24);

/// A duration must be at least 1 ms and at most [`MAX_DURATION`].
const fn check_duration(key: &'static str, value: Duration) -> Result<(), ConfigError> {
    if value.as_millis() == 0 {
        Err(ConfigError::ZeroDuration { key })
    } else if value.as_nanos() > MAX_DURATION.as_nanos() {
        Err(ConfigError::DurationTooLong { key })
    } else {
        Ok(())
    }
}

/// A duration key in milliseconds, checked by [`check_duration`].
fn millis(key: &'static str, value: Option<u64>) -> Result<Option<Duration>, ConfigError> {
    value.map_or(Ok(None), |ms| {
        let duration = Duration::from_millis(ms);
        check_duration(key, duration).map(|()| Some(duration))
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use autumn_web::config::MockEnv;

    #[test]
    fn default_path_prefers_the_manifest_dir() {
        let dir =
            std::env::temp_dir().join(format!("autumn-plugin-bevy-path-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("autumn.toml");
        std::fs::write(&file, "[bevy]\ntick_hz = 5\n").unwrap();
        let env = MockEnv::new().with("AUTUMN_MANIFEST_DIR", dir.to_str().unwrap());
        assert_eq!(default_path(&env), file);
        let loaded = BevyConfig::load(default_path(&env)).unwrap();
        assert_eq!(loaded.tick_mode, TickMode::Fixed(TickRate::new(5).unwrap()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn default_path_falls_back_to_the_current_directory() {
        let env = MockEnv::new().with("AUTUMN_MANIFEST_DIR", "/no/such/dir/for/bevy");
        assert_eq!(default_path(&env), std::path::PathBuf::from("autumn.toml"));
        assert_eq!(
            default_path(&MockEnv::new()),
            std::path::PathBuf::from("autumn.toml")
        );
    }
}
