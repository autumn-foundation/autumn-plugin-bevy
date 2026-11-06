//! Plugin identity, contract, wiring and conformance.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use autumn_plugin_bevy::prelude::*;
use autumn_web::actuator::HealthStatus;
use autumn_web::plugin::Plugin;
use autumn_web::plugin_conformance::{ConformanceConfig, run_conformance};
use autumn_web::test::TestApp;
use bevy_app::App;

use crate::support::counting;

// ── AC-26 ──────────────────────────────────────────────────────────────────

#[test]
fn plugin_identity_and_conformance() {
    let plugin = BevyPlugin::new(counting);
    assert_eq!(plugin.name(), "autumn-plugin-bevy");
    assert_eq!(BevyPlugin::NAME, env!("CARGO_PKG_NAME"));

    let contract = plugin.contract().expect("the plugin declares a contract");
    let label = contract.label();
    assert!(label.contains(env!("CARGO_PKG_NAME")), "{label}");
    assert!(label.contains(env!("CARGO_PKG_VERSION")), "{label}");
    let debug = format!("{contract:?}");
    assert!(
        debug.contains("\"0.10\""),
        "a third-party range, not lockstep: {debug}"
    );

    let builder = autumn_web::app().plugin(BevyPlugin::new(counting));
    assert!(builder.has_plugin("autumn-plugin-bevy"));
    assert!(builder.has_config_section("bevy"));
    let routes = builder.plugin_route_infos().expect("route infos");
    assert!(routes.is_empty(), "the plugin mounts no routes: {routes:?}");

    // The plugin mounts no routes. `no_routes` makes the harness accept
    // that, so every check must pass.
    let config = ConformanceConfig::new(BevyPlugin::NAME)
        .no_routes()
        .contract(contract);
    let report = run_conformance(&config, &routes);
    assert!(report.passed(), "{}", report.to_text_report());
}

#[tokio::test]
async fn duplicate_registration_runs_setup_once() {
    let first = Arc::new(AtomicUsize::new(0));
    let second = Arc::new(AtomicUsize::new(0));
    let (a, b) = (Arc::clone(&first), Arc::clone(&second));
    let _client = TestApp::new()
        .plugin(BevyPlugin::new(move |_app: &mut App| {
            a.fetch_add(1, Ordering::SeqCst);
        }))
        .plugin(BevyPlugin::new(move |_app: &mut App| {
            b.fetch_add(1, Ordering::SeqCst);
        }))
        .build();
    assert_eq!(first.load(Ordering::SeqCst), 1);
    assert_eq!(second.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn plugin_registers_health_metrics_and_handle() {
    let client = TestApp::new().plugin(BevyPlugin::new(counting)).build();
    let state = client.state();

    let handle = BevyHandle::from_state(state).expect("the plugin installs a handle");
    assert_eq!(handle.status(), RuntimeStatus::Running);

    let results = state.health_indicator_registry().run_all().await;
    let bevy = results
        .iter()
        .find(|r| r.name == "bevy")
        .expect("the bevy health indicator");
    assert_eq!(bevy.output.status, HealthStatus::Up);

    let sources = state.metrics_source_registry().collect_all();
    let (_, families) = sources
        .iter()
        .find(|(name, _)| name == "bevy")
        .expect("the bevy metrics source");
    assert!(families.iter().any(|f| f.name == "bevy_ticks_total"));
}

#[test]
fn plugin_debug_shows_setup_count() {
    let plugin = BevyPlugin::new(counting).setup(|_app: &mut App| {});
    let shown = format!("{plugin:?}");
    assert!(shown.contains("setups: 2"), "{shown}");
}
