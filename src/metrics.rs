//! The `bevy` metrics source.

use std::fmt;
use std::sync::Arc;

use std::sync::atomic::Ordering;

use autumn_web::actuator::{MetricFamily, MetricKind, MetricSample, MetricsSource};

use crate::shared::{Shared, Stats};
use crate::status::RuntimeStatus;

/// The metrics source of a [`BevyRuntime`](crate::BevyRuntime).
///
/// The families are `bevy_up`, `bevy_ticks_total`,
/// `bevy_tick_overruns_total`, `bevy_last_tick_seconds`,
/// `bevy_queue_depth`, `bevy_entities` and `bevy_commands_total` with the
/// label `outcome`. It reads counters only.
#[derive(Clone)]
pub struct BevyMetrics {
    shared: Arc<Shared>,
}

impl BevyMetrics {
    pub(crate) const fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }
}

impl MetricsSource for BevyMetrics {
    fn collect(&self) -> Vec<MetricFamily> {
        let shared = &self.shared;
        let stats = &shared.stats;
        let up = shared.status() == RuntimeStatus::Running;
        #[allow(clippy::cast_precision_loss)]
        let count = |counter: &std::sync::atomic::AtomicU64| Stats::get(counter) as f64;
        #[allow(clippy::cast_precision_loss)]
        let last_tick = Stats::get(&stats.last_tick_nanos) as f64 / 1e9;
        #[allow(clippy::cast_precision_loss)]
        let depth = stats.queue_depth.load(Ordering::Relaxed) as f64;
        let outcomes = [
            ("ok", count(&stats.commands_ok)),
            ("panicked", count(&stats.commands_panicked)),
            ("busy", count(&stats.commands_busy)),
            ("cancelled", count(&stats.commands_cancelled)),
            ("timeout", count(&stats.commands_timeout)),
            ("rejected", count(&stats.commands_rejected)),
        ];
        vec![
            plain(
                "bevy_up",
                "1 when the Bevy runtime is running",
                MetricKind::Gauge,
                f64::from(u8::from(up)),
            ),
            plain(
                "bevy_ticks_total",
                "Bevy updates that started",
                MetricKind::Counter,
                count(&stats.ticks),
            ),
            plain(
                "bevy_tick_overruns_total",
                "Fixed-rate ticks that the runtime skipped because it was late",
                MetricKind::Counter,
                count(&stats.overruns),
            ),
            plain(
                "bevy_last_tick_seconds",
                "Duration of the last Bevy update",
                MetricKind::Gauge,
                last_tick,
            ),
            plain(
                "bevy_queue_depth",
                "Commands that wait in the queue",
                MetricKind::Gauge,
                depth,
            ),
            plain(
                "bevy_entities",
                "Entities in the world after the last update, with the Bevy internal entities",
                MetricKind::Gauge,
                count(&stats.entities),
            ),
            MetricFamily {
                name: "bevy_commands_total".to_owned(),
                help: "Bevy commands by outcome".to_owned(),
                kind: MetricKind::Counter,
                samples: outcomes
                    .into_iter()
                    .map(|(outcome, value)| MetricSample {
                        labels: vec![("outcome".to_owned(), outcome.to_owned())],
                        value,
                    })
                    .collect(),
            },
        ]
    }
}

/// A family with one sample and no labels.
fn plain(name: &str, help: &str, kind: MetricKind, value: f64) -> MetricFamily {
    MetricFamily {
        name: name.to_owned(),
        help: help.to_owned(),
        kind,
        samples: vec![MetricSample {
            labels: Vec::new(),
            value,
        }],
    }
}

impl fmt::Debug for BevyMetrics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BevyMetrics")
            .field("runtime", &self.shared.id)
            .finish()
    }
}
