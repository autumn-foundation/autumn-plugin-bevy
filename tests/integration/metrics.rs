//! The `bevy` metrics source.

use std::collections::BTreeSet;

use autumn_plugin_bevy::prelude::*;
use autumn_web::actuator::{MetricFamily, MetricKind, MetricsSource};

use crate::support::{counting, manual, ready};

const FAMILIES: [&str; 7] = [
    "bevy_commands_total",
    "bevy_entities",
    "bevy_last_tick_seconds",
    "bevy_queue_depth",
    "bevy_tick_overruns_total",
    "bevy_ticks_total",
    "bevy_up",
];

const OUTCOMES: [&str; 6] = ["busy", "cancelled", "ok", "panicked", "rejected", "timeout"];

fn value(families: &[MetricFamily], name: &str, outcome: Option<&str>) -> f64 {
    let family = families
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("no family {name}"));
    family
        .samples
        .iter()
        .find(|s| {
            outcome.map_or(s.labels.is_empty(), |o| {
                s.labels.iter().any(|(k, v)| k == "outcome" && v == o)
            })
        })
        .unwrap_or_else(|| panic!("no sample {name} {outcome:?}"))
        .value
}

// ── AC-21 ──────────────────────────────────────────────────────────────────

#[test]
fn metric_families_are_fixed() {
    let runtime = BevyRuntime::new(manual(), counting);
    let families = runtime.metrics().collect();
    let names: BTreeSet<&str> = families.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, FAMILIES.into_iter().collect());
    for family in &families {
        assert!(!family.name.starts_with("autumn_"), "{}", family.name);
        assert!(!family.help.is_empty(), "{}", family.name);
        let counter = family.name.ends_with("_total");
        assert_eq!(
            matches!(family.kind, MetricKind::Counter),
            counter,
            "{}: a counter name ends in _total",
            family.name
        );
        for sample in &family.samples {
            for (key, label) in &sample.labels {
                assert_eq!(key, "outcome", "{}", family.name);
                assert!(OUTCOMES.contains(&label.as_str()), "{label}");
            }
        }
    }
    let commands = families
        .iter()
        .find(|f| f.name == "bevy_commands_total")
        .unwrap();
    let outcomes: BTreeSet<String> = commands
        .samples
        .iter()
        .flat_map(|s| s.labels.iter().map(|(_, v)| v.clone()))
        .collect();
    assert_eq!(outcomes, OUTCOMES.iter().map(|s| (*s).to_owned()).collect());
    assert!(value(&families, "bevy_up", None).abs() < f64::EPSILON);
}

#[tokio::test]
async fn metrics_count_ticks_and_commands() {
    let runtime = ready(BevyRuntime::new(manual(), counting)).await;
    let handle = runtime.handle();
    let metrics = runtime.metrics();
    let before = metrics.collect();
    let ticks_before = value(&before, "bevy_ticks_total", None);

    handle.step(2).await.unwrap();
    for _ in 0..3 {
        handle.with_world(|w| w.entity_count()).await.unwrap();
    }
    let _ = handle.with_world(|_w| -> u8 { panic!("count me") }).await;

    let after = metrics.collect();
    assert!((value(&after, "bevy_ticks_total", None) - ticks_before - 2.0).abs() < f64::EPSILON);
    // Three commands and one finished step.
    assert!((value(&after, "bevy_commands_total", Some("ok")) - 4.0).abs() < f64::EPSILON);
    assert!((value(&after, "bevy_commands_total", Some("panicked")) - 1.0).abs() < f64::EPSILON);
    assert!(value(&after, "bevy_queue_depth", None).abs() < f64::EPSILON);
    assert!((value(&after, "bevy_up", None) - 1.0).abs() < f64::EPSILON);
    assert!(value(&after, "bevy_last_tick_seconds", None) >= 0.0);
    runtime.shutdown().await.unwrap();
}
