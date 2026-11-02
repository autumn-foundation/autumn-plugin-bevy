//! The `bevy` health indicator.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use autumn_plugin_bevy::prelude::*;
use autumn_web::actuator::{HealthCheckOutput, HealthIndicator, HealthStatus, MetricsSource};
use bevy_app::App;

use crate::support::{Gate, counting, entered, eventually, manual, ready};

fn keys(output: &HealthCheckOutput) -> BTreeSet<String> {
    output.details.keys().cloned().collect()
}

fn reason(output: &HealthCheckOutput) -> Option<String> {
    output
        .details
        .get("reason")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
}

const UP_KEYS: [&str; 4] = ["busy_ms", "queue_depth", "status", "ticks"];

// ── AC-19 ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn health_tracks_lifecycle() {
    let runtime = BevyRuntime::new(manual(), counting);
    let health = runtime.health();

    let before = health.check().await;
    assert_eq!(before.status, HealthStatus::Down);
    assert_eq!(reason(&before).as_deref(), Some("not_started"));

    let runtime = ready(runtime).await;
    let up = health.check().await;
    assert_eq!(up.status, HealthStatus::Up, "{:?}", up.details);
    assert_eq!(keys(&up), UP_KEYS.iter().map(|s| (*s).to_owned()).collect());
    assert_eq!(up.details["status"], "running");
    assert_eq!(up.details["ticks"], 1);

    runtime.shutdown().await.unwrap();
    let stopped = health.check().await;
    assert_eq!(stopped.status, HealthStatus::Down);
    assert_eq!(reason(&stopped).as_deref(), Some("stopped"));
    let mut down_keys: BTreeSet<String> = UP_KEYS.iter().map(|s| (*s).to_owned()).collect();
    down_keys.insert("reason".to_owned());
    assert_eq!(keys(&stopped), down_keys);

    let failing = BevyRuntime::new(manual(), |_app: &mut App| panic!("boom"));
    let failing_health = failing.health();
    failing.start().unwrap();
    let _ = failing.wait_ready().await;
    assert!(
        eventually(Duration::from_secs(1), || failing.status()
            == RuntimeStatus::Failed)
        .await
    );
    assert_eq!(
        reason(&failing_health.check().await).as_deref(),
        Some("failed")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_is_down_when_stalled() {
    let config = manual().with_stall_threshold(Duration::from_millis(50));
    let runtime = ready(BevyRuntime::new(config, counting)).await;
    let health = runtime.health();
    assert_eq!(health.check().await.status, HealthStatus::Up);

    let (gate, blocked) = Gate::hold(&runtime.handle());
    let gate = entered(gate).await;
    tokio::time::sleep(Duration::from_millis(120)).await;
    let stalled = health.check().await;
    assert_eq!(stalled.status, HealthStatus::Down);
    assert_eq!(reason(&stalled).as_deref(), Some("stalled"));

    gate.open();
    blocked.await.unwrap().unwrap();
    let mut up = false;
    for _ in 0..100 {
        if health.check().await.status == HealthStatus::Up {
            up = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(up, "the check must be up again after the stall");
    runtime.shutdown().await.unwrap();
}

// ── AC-20 ──────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn check_and_collect_never_wait_on_world() {
    let runtime = ready(BevyRuntime::new(manual(), counting)).await;
    let (gate, blocked) = Gate::hold(&runtime.handle());
    let gate = entered(gate).await;

    let health = runtime.health();
    let metrics = runtime.metrics();
    let started = Instant::now();
    let _ = health.check().await;
    let families = metrics.collect();
    assert!(
        started.elapsed() < Duration::from_millis(50),
        "{:?}",
        started.elapsed()
    );
    assert!(!families.is_empty());

    gate.open();
    blocked.await.unwrap().unwrap();
    runtime.shutdown().await.unwrap();
}

#[test]
fn indicator_is_in_the_readiness_group() {
    let runtime = BevyRuntime::new(manual(), counting);
    let health = runtime.health();
    assert_eq!(
        health.group(),
        autumn_web::actuator::IndicatorGroup::Readiness
    );
    assert!(health.timeout_ms() > 0);
}
