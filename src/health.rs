//! The `bevy` health indicator.

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use std::sync::atomic::Ordering;

use autumn_web::actuator::{HealthCheckOutput, HealthIndicator};
use serde_json::Value;

use crate::shared::{Shared, Stats};
use crate::status::RuntimeStatus;

/// The health indicator of a [`BevyRuntime`](crate::BevyRuntime).
///
/// It is up only when the status is `Running` and the thread does not work
/// on one update or one command for longer than
/// [`BevyConfig::stall_threshold`](crate::BevyConfig::stall_threshold). It
/// reads counters only, so it never waits on the Bevy thread.
#[derive(Clone)]
pub struct BevyHealth {
    shared: Arc<Shared>,
}

/// The result of a health check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verdict {
    pub(crate) up: bool,
    pub(crate) reason: Option<&'static str>,
}

/// The health rule. Pure, so that the tests can check each case.
pub const fn evaluate(
    status: RuntimeStatus,
    working_for: Duration,
    threshold: Duration,
) -> Verdict {
    let reason = match status {
        RuntimeStatus::Running => {
            if working_for.as_nanos() < threshold.as_nanos() {
                return Verdict {
                    up: true,
                    reason: None,
                };
            }
            "stalled"
        }
        other => other.as_str(),
    };
    Verdict {
        up: false,
        reason: Some(reason),
    }
}

impl BevyHealth {
    pub(crate) const fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    /// Runs the check now.
    #[must_use]
    pub fn snapshot(&self) -> HealthCheckOutput {
        let shared = &self.shared;
        let status = shared.status();
        let working_for = shared.working_for();
        let verdict = evaluate(status, working_for, shared.config.stall_threshold);
        let mut details = HashMap::from([
            ("status".to_owned(), Value::from(status.as_str())),
            (
                "ticks".to_owned(),
                Value::from(Stats::get(&shared.stats.ticks)),
            ),
            (
                "queue_depth".to_owned(),
                Value::from(shared.stats.queue_depth.load(Ordering::Relaxed)),
            ),
            (
                "working_ms".to_owned(),
                Value::from(u64::try_from(working_for.as_millis()).unwrap_or(u64::MAX)),
            ),
        ]);
        let output = if verdict.up {
            HealthCheckOutput::up()
        } else {
            if let Some(reason) = verdict.reason {
                details.insert("reason".to_owned(), Value::from(reason));
            }
            HealthCheckOutput::down()
        };
        output.with_details(details)
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
        for (status, working, up, reason) in rows {
            let verdict = evaluate(status, working, LIMIT);
            assert_eq!(verdict.up, up, "{status:?} working {working:?}");
            assert_eq!(verdict.reason, reason, "{status:?} working {working:?}");
        }
    }

    #[test]
    fn up_only_when_running() {
        for &status in RuntimeStatus::ALL {
            if evaluate(status, Duration::ZERO, LIMIT).up {
                assert_eq!(status, Running);
            }
        }
    }
}
