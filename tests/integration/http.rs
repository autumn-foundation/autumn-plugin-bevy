//! The plugin in an Autumn app, through `TestApp` and HTTP requests.

use std::time::Duration;

use autumn_plugin_bevy::prelude::*;
use autumn_web::AutumnResult;
use autumn_web::prelude::*;
use autumn_web::test::TestApp;
use bevy_app::{App, AppExit, Startup, Update};
use bevy_ecs::prelude::*;

use crate::support::{Marker, Updates, counting, current_thread, manual};

#[get("/count")]
async fn count(bevy: Bevy) -> AutumnResult<String> {
    let n = bevy
        .with_world(|w| {
            let mut q = w.query_filtered::<(), With<Marker>>();
            q.iter(w).count()
        })
        .await?;
    Ok(n.to_string())
}

#[post("/spawn-marker")]
async fn spawn_marker(bevy: Bevy) -> AutumnResult<&'static str> {
    bevy.with_world(|w| {
        w.spawn(Marker);
    })
    .await?;
    Ok("ok")
}

#[get("/explode")]
async fn explode(bevy: Bevy) -> AutumnResult<String> {
    bevy.with_world(|_w| -> String { panic!("secret-xyz") })
        .await?;
    Ok(String::new())
}

#[get("/exit")]
async fn exit(bevy: Bevy) -> AutumnResult<&'static str> {
    bevy.with_world(|w| {
        w.write_message(AppExit::Success);
    })
    .await?;
    bevy.step(1).await?;
    Ok("exiting")
}

#[get("/ping")]
async fn ping() -> &'static str {
    "pong"
}

fn three_markers(app: &mut App) {
    app.add_systems(Startup, |mut commands: Commands| {
        for _ in 0..3 {
            commands.spawn(Marker);
        }
    });
}

// ── AC-01 ──────────────────────────────────────────────────────────────────

#[test]
fn testapp_build_waits_for_startup_schedule() {
    let body = current_thread(Duration::from_secs(5), || async {
        let client = TestApp::new()
            .plugin(BevyPlugin::new(three_markers))
            .routes(routes![count])
            .build();
        let response = client.get("/count").send().await;
        response.assert_ok();
        response.text()
    });
    assert_eq!(body, "3");
}

// ── AC-04 ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn handler_mutates_and_reads_world() {
    let client = TestApp::new()
        .plugin(BevyPlugin::new(|_app: &mut App| {}))
        .routes(routes![count, spawn_marker])
        .build();
    client
        .get("/count")
        .send()
        .await
        .assert_ok()
        .assert_body_eq("0");
    client.post("/spawn-marker").send().await.assert_ok();
    client
        .get("/count")
        .send()
        .await
        .assert_ok()
        .assert_body_eq("1");
}

// ── AC-06 ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn extractor_without_plugin_returns_500() {
    let client = TestApp::new().routes(routes![count, ping]).build();
    let response = client.get("/count").send().await;
    response.assert_status(500);
    // Autumn redacts the detail of a 5xx. Our text is fixed too.
    response.assert_body_contains("Internal server error");
    assert!(
        !response.text().contains("BevyPlugin"),
        "{}",
        response.text()
    );
    client.get("/ping").send().await.assert_ok();
}

// ── AC-07 ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn panic_text_is_not_in_response_body() {
    let client = TestApp::new()
        .plugin(BevyPlugin::new(|_app: &mut App| {}))
        .routes(routes![explode, count])
        .build();
    let response = client.get("/explode").send().await;
    response.assert_status(500);
    assert!(
        !response.text().contains("secret-xyz"),
        "{}",
        response.text()
    );
    client.get("/count").send().await.assert_ok();
}

// ── AC-12 ──────────────────────────────────────────────────────────────────

#[test]
#[should_panic(expected = "Plugin startup hook failed")]
fn setup_panic_fails_testapp_build() {
    current_thread(Duration::from_secs(10), || async {
        let _client = TestApp::new()
            .plugin(BevyPlugin::new(|_app: &mut App| panic!("boom in setup")))
            .routes(routes![ping])
            .build();
    });
}

// ── AC-14 ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn app_exit_keeps_server_serving() {
    let client = TestApp::new()
        .plugin(BevyPlugin::new(counting).config(manual()))
        .routes(routes![exit, count, ping])
        .build();
    let response = client.get("/exit").send().await;
    // The step may see the stop first; either answer is fine. The server
    // must go on.
    assert!(
        matches!(response.status.as_u16(), 200 | 503),
        "{}",
        response.status
    );
    let after = client.get("/count").send().await;
    after.assert_status(503);
    after.assert_body_contains("Service unavailable");
    client
        .get("/ping")
        .send()
        .await
        .assert_ok()
        .assert_body_eq("pong");
}

// ── Setup steps and config through the plugin ──────────────────────────────

#[get("/updates")]
async fn updates(bevy: Bevy) -> AutumnResult<String> {
    let n = bevy.with_world(|w| w.resource::<Updates>().0).await?;
    Ok(n.to_string())
}

#[tokio::test]
async fn setup_steps_run_in_order_with_config() {
    let client = TestApp::new()
        .plugin(
            BevyPlugin::new(|app: &mut App| {
                app.insert_resource(Updates(100));
            })
            .setup(|app: &mut App| {
                // The second step sees the first.
                let n = app.world().resource::<Updates>().0;
                app.insert_resource(Updates(n + 1));
                app.add_systems(Update, crate::support::count_updates);
            })
            .config(manual()),
        )
        .routes(routes![updates])
        .build();
    // 101 after setup, plus the boot update.
    client
        .get("/updates")
        .send()
        .await
        .assert_ok()
        .assert_body_eq("102");
}
