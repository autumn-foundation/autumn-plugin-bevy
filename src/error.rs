//! Errors, and how they map to HTTP responses.

use std::time::Duration;

use autumn_web::AutumnError;
use autumn_web::reexports::http::StatusCode;

use crate::status::RuntimeStatus;

/// An error from the Bevy runtime.
///
/// The `Display` text is for logs. HTTP responses use only
/// [`RuntimeError::public_message`], so a client never sees panic text.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RuntimeError {
    /// The app did not register [`BevyPlugin`](crate::BevyPlugin).
    #[error("BevyPlugin is not registered")]
    NotInstalled,
    /// The runtime does not accept commands in this status.
    #[error("bevy runtime is not running (status: {0})")]
    NotRunning(RuntimeStatus),
    /// The command queue is full. Try again later.
    #[error("bevy command queue is full")]
    Busy,
    /// The command did not finish in time. If it did not start, it does not
    /// run.
    #[error("bevy command did not finish within {0:?}")]
    Timeout(Duration),
    /// The command panicked. The runtime continues.
    #[error("bevy command panicked")]
    CommandPanicked,
    /// A call from the Bevy thread into its own runtime. It would deadlock.
    #[error("a call from the bevy thread into its own runtime would deadlock")]
    Reentrant,
    /// [`BevyHandle::step`](crate::BevyHandle::step) needs
    /// [`TickMode::Manual`](crate::TickMode::Manual).
    #[error("step needs TickMode::Manual")]
    WrongMode,
    /// [`BevyRuntime::start`](crate::BevyRuntime::start) was called two times.
    #[error("bevy runtime is already started")]
    AlreadyStarted,
    /// The operating system did not start the thread.
    #[error("cannot spawn the bevy thread")]
    Spawn(#[source] std::io::Error),
    /// Setup or the first update panicked.
    #[error("bevy setup panicked: {0}")]
    SetupPanicked(String),
    /// Setup and the first update did not finish in time.
    #[error("bevy runtime was not ready within {0:?}")]
    StartupTimeout(Duration),
    /// The thread did not stop in time.
    #[error("bevy runtime did not stop within {0:?}")]
    ShutdownTimeout(Duration),
}

impl RuntimeError {
    /// The HTTP status for this error: 503 when a retry can help, else 500.
    #[must_use]
    pub const fn status_code(&self) -> StatusCode {
        StatusCode::OK
    }

    /// The fixed text for an HTTP body. It has no internal data.
    #[must_use]
    pub const fn public_message(&self) -> &'static str {
        ""
    }

    /// The Autumn error with the status of [`RuntimeError::status_code`] and
    /// the text of [`RuntimeError::public_message`].
    ///
    /// The [`Bevy`](crate::Bevy) extractor methods use it. Do not use `?` on
    /// a `RuntimeError` in a handler: the `From` impl of autumn-web makes every
    /// error a 500 with its `Display` text.
    #[must_use]
    pub fn into_autumn_error(self) -> AutumnError {
        AutumnError::internal_server_error_msg("")
    }
}

/// An error in the Bevy configuration. The text names the key.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// `tick_hz` is out of range.
    #[error("bevy.tick_hz must be in 1..=1000, got {0}")]
    TickRate(u32),
    /// `queue_capacity` is out of range.
    #[error("bevy.queue_capacity must be in 1..=65536, got {0}")]
    QueueCapacity(usize),
    /// A duration key is 0.
    #[error("bevy.{key} must be at least 1 ms")]
    ZeroDuration {
        /// The key.
        key: &'static str,
    },
    /// The TOML text is not valid, or the `[bevy]` table has an unknown key
    /// or a wrong type.
    #[error("invalid [bevy] section: {0}")]
    Parse(String),
    /// The file exists, but it cannot be read.
    #[error("cannot read {path}: {message}")]
    Read {
        /// The file path.
        path: String,
        /// The operating system error.
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_errors() -> Vec<RuntimeError> {
        vec![
            RuntimeError::NotInstalled,
            RuntimeError::NotRunning(RuntimeStatus::Stopped),
            RuntimeError::Busy,
            RuntimeError::Timeout(Duration::from_millis(5)),
            RuntimeError::CommandPanicked,
            RuntimeError::Reentrant,
            RuntimeError::WrongMode,
            RuntimeError::AlreadyStarted,
            RuntimeError::Spawn(std::io::Error::other("no threads")),
            RuntimeError::SetupPanicked("secret-setup".to_owned()),
            RuntimeError::StartupTimeout(Duration::from_secs(1)),
            RuntimeError::ShutdownTimeout(Duration::from_secs(1)),
        ]
    }

    #[test]
    fn status_mapping_table() {
        let unavailable = StatusCode::SERVICE_UNAVAILABLE;
        let internal = StatusCode::INTERNAL_SERVER_ERROR;
        let expected = [
            (internal, "internal error"),
            (unavailable, "bevy runtime is unavailable"),
            (unavailable, "bevy runtime is busy"),
            (unavailable, "bevy runtime is unavailable"),
            (internal, "internal error"),
            (internal, "internal error"),
            (internal, "internal error"),
            (internal, "internal error"),
            (internal, "internal error"),
            (unavailable, "bevy runtime is unavailable"),
            (unavailable, "bevy runtime is unavailable"),
            (unavailable, "bevy runtime is unavailable"),
        ];
        for (err, (status, message)) in all_errors().iter().zip(expected) {
            assert_eq!(err.status_code(), status, "{err:?}");
            assert_eq!(err.public_message(), message, "{err:?}");
        }
    }

    #[test]
    fn autumn_error_keeps_status_and_hides_details() {
        for err in all_errors() {
            let status = err.status_code();
            let shown = format!("{err}");
            let autumn = err.into_autumn_error();
            assert_eq!(autumn.status(), status);
            let text = autumn.to_string();
            assert!(!text.contains("secret-setup"), "{text}");
            if shown.contains("secret-setup") {
                assert_eq!(text, "bevy runtime is unavailable");
            }
        }
    }

    #[test]
    fn config_errors_name_the_key() {
        assert!(ConfigError::TickRate(0).to_string().contains("tick_hz"));
        assert!(
            ConfigError::QueueCapacity(0)
                .to_string()
                .contains("queue_capacity")
        );
        let zero = ConfigError::ZeroDuration {
            key: "startup_timeout_ms",
        };
        assert!(zero.to_string().contains("startup_timeout_ms"));
    }
}
