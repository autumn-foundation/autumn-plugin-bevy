//! Runtime status and runtime identity.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// The lifecycle status of a [`BevyRuntime`](crate::BevyRuntime).
///
/// The status goes in one direction only. `Stopped` and `Failed` are the
/// end. See the "Lifecycle" section of the README and `src/lifecycle.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RuntimeStatus {
    /// The thread is not started.
    NotStarted,
    /// The thread runs setup and the first update.
    Starting,
    /// The thread runs updates and commands.
    Running,
    /// The thread runs the last update, rejects the queued commands and drops
    /// the world.
    Stopping,
    /// The runtime stopped with no failure.
    Stopped,
    /// The runtime stopped after a failure: a panic, `AppExit::Error`, a
    /// startup timeout, an invalid setup or a spawn error.
    Failed,
}

impl RuntimeStatus {
    /// All statuses, in lifecycle order. A later release can add a status.
    pub const ALL: &'static [Self] = &[
        Self::NotStarted,
        Self::Starting,
        Self::Running,
        Self::Stopping,
        Self::Stopped,
        Self::Failed,
    ];

    /// `true` for `Stopped` and `Failed`. No status comes after these.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Stopped | Self::Failed)
    }

    /// `true` when a new command can go into the queue: `Starting` and
    /// `Running`.
    #[must_use]
    pub const fn accepts_commands(self) -> bool {
        matches!(self, Self::Starting | Self::Running)
    }

    /// The `snake_case` name. Health details and logs use it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotStarted => "not_started",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
        }
    }

    /// The code that the lock-free status mirror stores.
    pub(crate) const fn to_u8(self) -> u8 {
        match self {
            Self::NotStarted => 0,
            Self::Starting => 1,
            Self::Running => 2,
            Self::Stopping => 3,
            Self::Stopped => 4,
            Self::Failed => 5,
        }
    }

    /// The reverse of [`Self::to_u8`]. An unknown code gives `Failed`.
    pub(crate) const fn from_u8(code: u8) -> Self {
        match code {
            0 => Self::NotStarted,
            1 => Self::Starting,
            2 => Self::Running,
            3 => Self::Stopping,
            4 => Self::Stopped,
            _ => Self::Failed,
        }
    }
}

impl fmt::Display for RuntimeStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The identity of one [`BevyRuntime`](crate::BevyRuntime) in this process.
///
/// The re-entry check uses it: a call from a runtime thread into the same
/// runtime fails with [`RuntimeError::Reentrant`](crate::RuntimeError::Reentrant).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RuntimeId(u64);

impl RuntimeId {
    /// Makes a new identity. Each call gives a different value.
    pub(crate) fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }

    /// The number value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for RuntimeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bevy-runtime-{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_and_accepting_sets_are_exact() {
        let terminal: Vec<_> = RuntimeStatus::ALL
            .iter()
            .copied()
            .filter(|s| s.is_terminal())
            .collect();
        assert_eq!(terminal, [RuntimeStatus::Stopped, RuntimeStatus::Failed]);
        let accepting: Vec<_> = RuntimeStatus::ALL
            .iter()
            .copied()
            .filter(|s| s.accepts_commands())
            .collect();
        assert_eq!(accepting, [RuntimeStatus::Starting, RuntimeStatus::Running]);
    }

    #[test]
    fn names_are_snake_case_and_unique() {
        let names: Vec<_> = RuntimeStatus::ALL
            .iter()
            .copied()
            .map(RuntimeStatus::as_str)
            .collect();
        assert_eq!(
            names,
            [
                "not_started",
                "starting",
                "running",
                "stopping",
                "stopped",
                "failed"
            ]
        );
        assert_eq!(RuntimeStatus::Running.to_string(), "running");
    }

    #[test]
    fn mirror_code_round_trips() {
        for &status in RuntimeStatus::ALL {
            assert_eq!(RuntimeStatus::from_u8(status.to_u8()), status);
        }
        assert_eq!(RuntimeStatus::from_u8(200), RuntimeStatus::Failed);
    }

    #[test]
    fn runtime_ids_are_unique() {
        let a = RuntimeId::next();
        let b = RuntimeId::next();
        assert_ne!(a, b);
        assert!(a.to_string().starts_with("bevy-runtime-"));
    }
}
