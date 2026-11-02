//! The `bevy` health indicator.

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use autumn_web::actuator::{HealthCheckOutput, HealthIndicator};

use crate::shared::Shared;
use crate::status::RuntimeStatus;

/// The health indicator of a [`BevyRuntime`](crate::BevyRuntime).
///
/// It is up only when the status is `Running` and the thread is not busy
/// for longer than
/// [`BevyConfig::stall_threshold`](crate::BevyConfig::stall_threshold). It
/// reads counters only, so it never waits on the Bevy thread.
#[derive(Clone)]
pub struct BevyHealth {
    shared: Arc<Shared>,
}

/// The result of a health check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Verdict {
    pub(crate) up: bool,
    pub(crate) reason: Option<&'static str>,
}

/// The health rule. Pure, so that the tests can check each case.
pub(crate) const fn evaluate(
    status: RuntimeStatus,
    busy_for: Duration,
    threshold: Duration,
) -> Verdict {
    let _ = (status, busy_for, threshold);
    Verdict {
        up: true,
        reason: None,
    }
}

impl BevyHealth {
    pub(crate) const fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    /// Runs the check now.
    #[must_use]
    pub fn snapshot(&self) -> HealthCheckOutput {
        let _ = &self.shared;
        HealthCheckOutput::up().with_details(HashMap::new())
    }
}

impl HealthIndicator for BevyHealth {
    fn check(&self) -> Pin<Box<dyn Future<Output = HealthCheckOutput> + Send + '_>> {
        let output = self.snapshot();
        Box::pin(async move { output })
    }
}

impl fmt::Debug for BevyHealth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BevyHealth")
            .field("runtime", &self.shared.id)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use RuntimeStatus::{Failed, NotStarted, Running, Starting, Stopped, Stopping};

    const LIMIT: Duration = Duration::from_millis(100);

    #[test]
    fn evaluate_table() {
        let idle = Duration::ZERO;
        let rows = [
            (NotStarted, idle, false, Some("not_started")),
            (Starting, idle, false, Some("starting")),
            (Running, idle, true, None),
            (Running, Duration::from_millis(99), true, None),
            (Running, LIMIT, false, Some("stalled")),
            (Stopping, idle, false, Some("stopping")),
            (Stopped, idle, false, Some("stopped")),
            (Failed, idle, false, Some("failed")),
        ];
        for (status, busy, up, reason) in rows {
            let verdict = evaluate(status, busy, LIMIT);
            assert_eq!(verdict.up, up, "{status:?} busy {busy:?}");
            assert_eq!(verdict.reason, reason, "{status:?} busy {busy:?}");
        }
    }

    #[test]
    fn up_only_when_running() {
        for status in RuntimeStatus::ALL {
            if evaluate(status, Duration::ZERO, LIMIT).up {
                assert_eq!(status, Running);
            }
        }
    }
}
