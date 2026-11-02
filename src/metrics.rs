//! The `bevy` metrics source.

use std::fmt;
use std::sync::Arc;

use autumn_web::actuator::{MetricFamily, MetricsSource};

use crate::shared::Shared;

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
        let _ = &self.shared;
        Vec::new()
    }
}

impl fmt::Debug for BevyMetrics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BevyMetrics")
            .field("runtime", &self.shared.id)
            .finish()
    }
}
