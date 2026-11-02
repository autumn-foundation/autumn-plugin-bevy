//! The `time` feature: `TimePlugin` and fixed time steps in manual mode.

use std::time::Duration;

use autumn_plugin_bevy::prelude::*;
use bevy_app::App;
use bevy_time::{Time, TimePlugin, TimeUpdateStrategy, Virtual};

use crate::support::{manual, ready};

// ── AC-24 ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn manual_time_advances_by_fixed_step() {
    let step = Duration::from_millis(10);
    let config = manual().with_manual_time_step(step);
    let runtime = ready(BevyRuntime::new(config, |_app: &mut App| {})).await;
    let handle = runtime.handle();

    let first = handle
        .with_world(|w| w.resource::<Time<Virtual>>().elapsed())
        .await
        .unwrap();
    handle.step(5).await.unwrap();
    let after = handle
        .with_world(|w| w.resource::<Time<Virtual>>().elapsed())
        .await
        .unwrap();
    assert_eq!(after.checked_sub(first).unwrap(), step * 5);
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn user_time_plugin_is_kept() {
    let runtime = ready(BevyRuntime::new(manual(), |app: &mut App| {
        app.add_plugins(TimePlugin);
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            40,
        )));
    }))
    .await;
    let handle = runtime.handle();
    let first = handle
        .with_world(|w| w.resource::<Time<Virtual>>().elapsed())
        .await
        .unwrap();
    handle.step(2).await.unwrap();
    let after = handle
        .with_world(|w| w.resource::<Time<Virtual>>().elapsed())
        .await
        .unwrap();
    assert_eq!(
        after.checked_sub(first).unwrap(),
        Duration::from_millis(80),
        "the app strategy is kept"
    );
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn fixed_mode_uses_the_real_clock() {
    let runtime = ready(BevyRuntime::new(BevyConfig::default(), |_app: &mut App| {})).await;
    let strategy = runtime
        .handle()
        .with_world(|w| {
            matches!(
                *w.resource::<TimeUpdateStrategy>(),
                TimeUpdateStrategy::Automatic
            )
        })
        .await
        .unwrap();
    assert!(strategy);
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn manual_mode_keeps_a_strategy_from_setup() {
    // Setup sets a strategy but adds no TimePlugin. The plugin adds the
    // plugin and keeps the strategy.
    let runtime = ready(BevyRuntime::new(manual(), |app: &mut App| {
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            25,
        )));
    }))
    .await;
    let handle = runtime.handle();
    let first = handle
        .with_world(|w| w.resource::<Time<Virtual>>().elapsed())
        .await
        .unwrap();
    handle.step(4).await.unwrap();
    let after = handle
        .with_world(|w| w.resource::<Time<Virtual>>().elapsed())
        .await
        .unwrap();
    assert_eq!(
        after.checked_sub(first).unwrap(),
        Duration::from_millis(100)
    );
    runtime.shutdown().await.unwrap();
}
