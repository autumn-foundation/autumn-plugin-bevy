//! Regression tests for the findings of the code review. Each test names its
//! finding.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use autumn_plugin_bevy::prelude::*;
use autumn_plugin_bevy::{BevyConfig, ConfigError};
use autumn_web::actuator::{HealthIndicator, HealthStatus};
use bevy_app::{App, Startup, Update};

use crate::support::{
    Gate, PanicOnDrop, block_on, counting, entered, eventually, manual, ready, within,
};

fn slow_updates(app: &mut App) {
    counting(app);
    app.add_systems(Update, || std::thread::sleep(Duration::from_millis(5)));
}

// ── F01: a long step reads the stop flag, the caller, and the stall rule ────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn f01_long_step_stops_on_shutdown() {
    let config = manual()
        .with_command_timeout(None)
        .with_shutdown_timeout(Duration::from_secs(3));
    let runtime = Arc::new(ready(BevyRuntime::new(config, slow_updates)).await);
    let handle = runtime.handle();
    let step = tokio::spawn(async move { handle.step(1_000_000).await });
    tokio::time::sleep(Duration::from_millis(50)).await;

    runtime.shutdown().await.expect("shutdown in time");
    assert!(step.await.unwrap().is_err());
    assert_eq!(runtime.status(), RuntimeStatus::Stopped);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn f01_long_step_ends_when_the_caller_times_out() {
    let config = manual().with_command_timeout(Some(Duration::from_millis(100)));
    let runtime = ready(BevyRuntime::new(config, slow_updates)).await;
    let handle = runtime.handle();
    let err = handle.step(1_000_000).await.unwrap_err();
    assert!(matches!(err, RuntimeError::Timeout(_)), "{err:?}");

    // The thread stops the step soon, so a new call gets an answer.
    let n = handle
        .with_world_timeout(Some(Duration::from_secs(2)), |w| {
            w.resource::<crate::support::Updates>().0
        })
        .await
        .expect("the thread ends the abandoned step");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let later = handle.ticks();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(handle.ticks(), later, "no update runs after the step ends");
    assert!(n < 1_000, "{n}");
    runtime.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn f01_long_step_shows_as_stalled() {
    let config = manual()
        .with_command_timeout(None)
        .with_stall_threshold(Duration::from_millis(50));
    let runtime = ready(BevyRuntime::new(config, slow_updates)).await;
    let handle = runtime.handle();
    let health = runtime.health();
    let step = tokio::spawn(async move { handle.step(60).await });
    tokio::time::sleep(Duration::from_millis(150)).await;
    let during = health.check().await;
    assert_eq!(during.status, HealthStatus::Down, "{:?}", during.details);
    assert_eq!(during.details["reason"], "stalled");
    step.await.unwrap().unwrap();
    runtime.shutdown().await.unwrap();
}

// ── F02: a panic in a drop on the Bevy thread is contained ──────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn f02_a_result_that_panics_on_drop_is_contained() {
    let runtime = ready(BevyRuntime::new(manual(), counting)).await;
    let handle = runtime.handle();
    // The caller goes away first, so the result drops on the Bevy thread.
    let err = handle
        .with_world_timeout(Some(Duration::from_millis(20)), |_w| {
            std::thread::sleep(Duration::from_millis(100));
            PanicOnDrop
        })
        .await
        .unwrap_err();
    assert!(matches!(err, RuntimeError::Timeout(_)));
    assert_eq!(handle.with_world(|_w| 1).await.unwrap(), 1);
    assert_eq!(runtime.status(), RuntimeStatus::Running);
    runtime.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn f02_a_closure_that_panics_on_drop_is_contained() {
    let runtime = ready(BevyRuntime::new(manual(), counting)).await;
    let handle = runtime.handle();
    let (gate, blocked) = Gate::hold(&handle);
    let gate = entered(gate).await;
    // This command waits behind the gate and times out, so it never runs.
    // The thread then drops its closure, and the drop panics.
    let guard = PanicOnDrop;
    let err = handle
        .with_world_timeout(Some(Duration::from_millis(20)), move |_w| drop(guard))
        .await
        .unwrap_err();
    assert!(matches!(err, RuntimeError::Timeout(_)));
    gate.open();
    blocked.await.unwrap().unwrap();
    assert_eq!(handle.with_world(|_w| 2).await.unwrap(), 2);
    assert_eq!(runtime.status(), RuntimeStatus::Running);

    // A rejected closure drops on the Bevy thread too.
    let (gate, blocked) = Gate::hold(&handle);
    let gate = entered(gate).await;
    let h = handle.clone();
    let guard = PanicOnDrop;
    let rejected = tokio::spawn(async move { h.with_world(move |_w| drop(guard)).await });
    assert!(eventually(Duration::from_secs(2), || handle.queue_depth() >= 1).await);
    let runtime = Arc::new(runtime);
    let stopper = Arc::clone(&runtime);
    let stop = tokio::spawn(async move { stopper.shutdown().await });
    // The stop's wake message shows that the stop flag is set.
    assert!(eventually(Duration::from_secs(5), || handle.queue_depth() >= 2).await);
    gate.open();
    blocked.await.unwrap().unwrap();
    let answer = rejected.await.unwrap();
    assert!(
        matches!(answer, Err(RuntimeError::NotRunning(_))),
        "{answer:?}"
    );
    stop.await.unwrap().unwrap();
    assert!(runtime.status().is_terminal());
}

// ── F03, F08, F15: durations are checked ────────────────────────────────────

#[tokio::test]
async fn f03_out_of_range_durations_are_rejected_at_start() {
    let cases = [
        BevyConfig::manual().with_startup_timeout(Duration::MAX),
        BevyConfig::manual().with_startup_timeout(Duration::ZERO),
        BevyConfig::manual().with_stall_threshold(Duration::ZERO),
        BevyConfig::manual().with_shutdown_timeout(Duration::MAX),
        BevyConfig::manual().with_command_timeout(Some(Duration::ZERO)),
        BevyConfig::manual().with_command_timeout(Some(Duration::MAX)),
        BevyConfig::manual().with_manual_time_step(Duration::ZERO),
    ];
    for config in cases {
        let shown = format!("{config:?}");
        assert!(config.validate().is_err(), "{shown}");
        let runtime = BevyRuntime::new(config, counting);
        let err = runtime.start().unwrap_err();
        assert!(
            matches!(err, RuntimeError::InvalidConfig(_)),
            "{shown}: {err:?}"
        );
        assert_eq!(runtime.status(), RuntimeStatus::NotStarted);
    }
    assert!(BevyConfig::default().validate().is_ok());
    assert!(
        BevyConfig::default()
            .with_command_timeout(None)
            .validate()
            .is_ok()
    );
}

#[test]
fn f03_toml_rejects_a_duration_above_the_limit() {
    let err = BevyConfig::from_toml_str("[bevy]\nstartup_timeout_ms = 18446744073709551615\n")
        .unwrap_err();
    assert!(
        matches!(
            err,
            ConfigError::DurationTooLong {
                key: "startup_timeout_ms"
            }
        ),
        "{err:?}"
    );
}

// ── F05: setup must not call App::run ───────────────────────────────────────

#[tokio::test]
async fn f05_setup_that_calls_app_run_fails() {
    let runtime = BevyRuntime::new(manual(), |app: &mut App| {
        counting(app);
        app.run();
    });
    runtime.start().unwrap();
    let err = runtime.wait_ready().await.unwrap_err();
    assert!(matches!(err, RuntimeError::InvalidSetup(_)), "{err:?}");
    assert!(err.to_string().contains("App::run"), "{err}");
    assert!(
        eventually(Duration::from_secs(1), || runtime.status()
            == RuntimeStatus::Failed)
        .await
    );
}

// ── F11, F16: calls work with no tokio runtime ──────────────────────────────

#[test]
fn f11_calls_work_with_no_tokio_runtime() {
    let value = within(Duration::from_secs(10), || {
        let runtime = BevyRuntime::new(manual(), counting);
        runtime.start().unwrap();
        block_on(runtime.wait_ready()).unwrap();
        let handle = runtime.handle();
        // The default config has a wait limit. With no tokio runtime the
        // call waits with no limit and does not panic.
        let value = block_on(handle.with_world(|_w| 42)).unwrap();
        block_on(runtime.shutdown()).unwrap();
        value
    });
    assert_eq!(value, 42);
}

// ── F13, F24, F40: a stop or a startup failure during boot ──────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn f24_shutdown_during_setup_never_reaches_running() {
    let startups = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&startups);
    let runtime = Arc::new(BevyRuntime::new(manual(), move |app: &mut App| {
        std::thread::sleep(Duration::from_millis(200));
        app.add_systems(Startup, move || {
            seen.fetch_add(1, Ordering::SeqCst);
        });
    }));
    runtime.start().unwrap();
    let waiter = Arc::clone(&runtime);
    let wait = tokio::spawn(async move { waiter.wait_ready().await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    runtime.shutdown().await.unwrap();
    assert_eq!(runtime.status(), RuntimeStatus::Stopped);
    assert!(wait.await.unwrap().is_err(), "a stopped boot is not ready");
    assert_eq!(
        startups.load(Ordering::SeqCst),
        0,
        "Startup must not run after a stop"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn f40_late_setup_after_a_startup_timeout_ends_failed() {
    let startups = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&startups);
    let config = manual().with_startup_timeout(Duration::from_millis(100));
    let runtime = BevyRuntime::new(config, move |app: &mut App| {
        std::thread::sleep(Duration::from_millis(300));
        app.add_systems(Startup, move || {
            seen.fetch_add(1, Ordering::SeqCst);
        });
    });
    let health = runtime.health();
    runtime.start().unwrap();
    let err = runtime.wait_ready().await.unwrap_err();
    assert!(matches!(err, RuntimeError::StartupTimeout(_)), "{err:?}");

    let saw_running = Arc::new(AtomicU64::new(0));
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && !runtime.status().is_terminal() {
        if runtime.status() == RuntimeStatus::Running {
            saw_running.fetch_add(1, Ordering::SeqCst);
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!(runtime.status(), RuntimeStatus::Failed);
    assert_eq!(
        saw_running.load(Ordering::SeqCst),
        0,
        "a timed-out boot never runs"
    );
    assert_eq!(
        startups.load(Ordering::SeqCst),
        0,
        "Startup must not run after a failure"
    );
    assert_eq!(health.check().await.details["reason"], "failed");
}

#[test]
fn f13_shutdown_racing_start_always_stops() {
    // Start and shutdown race on two threads. The old code could return Ok
    // from shutdown with no stop flag, and the thread then ran on.
    within(Duration::from_mins(1), || {
        for _ in 0..200 {
            let runtime = Arc::new(BevyRuntime::new(manual(), counting));
            let starter = Arc::clone(&runtime);
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let wait = Arc::clone(&barrier);
            let t = std::thread::spawn(move || {
                wait.wait();
                let _ = starter.start();
            });
            barrier.wait();
            block_on(runtime.shutdown()).expect("shutdown");
            t.join().unwrap();
            // A start that won the race must still end: shut down again.
            block_on(runtime.shutdown()).expect("shutdown");
            assert!(runtime.status().is_terminal(), "{:?}", runtime.status());
        }
    });
}

// ── F19: the queue keeps its accounting on the real runtime ────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn f19_every_accepted_call_gets_one_answer() {
    // A fixed pseudo-random mix of calls, steps, panics and a stop. Every
    // call ends, and the counters add up to the calls the queue took.
    let config = manual()
        .with_queue_capacity(autumn_plugin_bevy::QueueCapacity::new(32).unwrap())
        .with_command_timeout(Some(Duration::from_secs(5)));
    let runtime = Arc::new(ready(BevyRuntime::new(config, counting)).await);
    let handle = runtime.handle();
    let mut seed: u64 = 0x5eed;
    let mut tasks = Vec::new();
    for i in 0..400_u64 {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let h = handle.clone();
        let kind = (seed >> 33) % 4;
        tasks.push(tokio::spawn(async move {
            match kind {
                0 => h.step(1).await.map(drop),
                1 => h.with_world(|_w| -> () { panic!("mix boom") }).await,
                _ => {
                    h.with_world(|w| {
                        let _ = w.entity_count();
                    })
                    .await
                }
            }
        }));
        if i == 300 {
            // Let some of the mix run, then stop while calls still come.
            let stats = runtime.metrics();
            let ran = move || {
                let families = autumn_web::actuator::MetricsSource::collect(&stats);
                families
                    .iter()
                    .find(|f| f.name == "bevy_commands_total")
                    .map_or(0.0, |f| {
                        f.samples
                            .iter()
                            .filter(|s| s.labels[0].1 == "ok" || s.labels[0].1 == "panicked")
                            .map(|s| s.value)
                            .sum::<f64>()
                    })
            };
            assert!(eventually(Duration::from_secs(10), || ran() >= 20.0).await);
            let stopper = Arc::clone(&runtime);
            tokio::spawn(async move { stopper.shutdown().await.unwrap() });
        }
    }
    let (mut ok, mut panicked, mut busy, mut not_running) = (0.0, 0.0, 0.0, 0.0);
    for task in tasks {
        let answer = tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .expect("every call ends")
            .unwrap();
        match answer {
            Ok(()) => ok += 1.0,
            Err(RuntimeError::CommandPanicked) => panicked += 1.0,
            Err(RuntimeError::Busy) => busy += 1.0,
            Err(RuntimeError::NotRunning(_)) => not_running += 1.0,
            Err(other) => panic!("no call may end with {other:?}"),
        }
    }
    assert!(eventually(Duration::from_secs(5), || runtime.status().is_terminal()).await);
    let families = autumn_web::actuator::MetricsSource::collect(&runtime.metrics());
    let commands = families
        .iter()
        .find(|f| f.name == "bevy_commands_total")
        .unwrap();
    let get = |o: &str| {
        commands
            .samples
            .iter()
            .find(|s| s.labels[0].1 == o)
            .map_or(0.0, |s| s.value)
    };
    // Each answer has its count. A rejection in the drain counts as
    // `rejected`; a refusal at the status gate counts nothing.
    assert!(
        (get("ok") - ok).abs() < f64::EPSILON,
        "ok {} vs {ok}",
        get("ok")
    );
    assert!((get("panicked") - panicked).abs() < f64::EPSILON);
    assert!((get("busy") - busy).abs() < f64::EPSILON);
    assert!(
        get("rejected") <= not_running,
        "rejected {} vs {not_running}",
        get("rejected")
    );
    assert!(get("cancelled").abs() < f64::EPSILON && get("timeout").abs() < f64::EPSILON);
    assert!(
        ok > 0.0 && panicked > 0.0,
        "the mix ran: ok {ok}, panicked {panicked}"
    );
}

// ── R01: a setup may put a new App in place ─────────────────────────────────

#[tokio::test]
async fn r01_setup_that_replaces_the_app_boots() {
    let runtime = ready(BevyRuntime::new(manual(), |app: &mut App| {
        let mut fresh = App::new();
        counting(&mut fresh);
        *app = fresh;
    }))
    .await;
    let n = runtime
        .handle()
        .with_world(|w| w.resource::<crate::support::Updates>().0)
        .await
        .unwrap();
    assert_eq!(n, 1);
    runtime.shutdown().await.unwrap();
}

// ── R03: step outcomes ──────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r03_step_outcomes_are_counted() {
    let config = manual().with_command_timeout(Some(Duration::from_millis(100)));
    let runtime = ready(BevyRuntime::new(config, slow_updates)).await;
    let handle = runtime.handle();
    handle.step(2).await.unwrap();
    // The caller leaves while updates run: the step ran, so it is `ok`.
    let _ = handle.step(1_000_000).await;
    handle
        .with_world_timeout(Some(Duration::from_secs(5)), |_w| ())
        .await
        .unwrap();
    let families = autumn_web::actuator::MetricsSource::collect(&runtime.metrics());
    let commands = families
        .iter()
        .find(|f| f.name == "bevy_commands_total")
        .unwrap();
    let get = |o: &str| {
        commands
            .samples
            .iter()
            .find(|s| s.labels[0].1 == o)
            .unwrap()
            .value
    };
    assert!(
        (get("ok") - 3.0).abs() < f64::EPSILON,
        "two steps and one call: {}",
        get("ok")
    );
    assert!(get("cancelled").abs() < f64::EPSILON);
    assert!((get("timeout") - 1.0).abs() < f64::EPSILON);
    runtime.shutdown().await.unwrap();
}

// ── R16: a panic payload that panics in its drop ────────────────────────────

#[tokio::test]
async fn r16_a_panic_payload_that_panics_on_drop_is_contained() {
    let runtime = ready(BevyRuntime::new(manual(), counting)).await;
    let handle = runtime.handle();
    let err = handle
        .with_world(|_w| -> () { std::panic::panic_any(PanicOnDrop) })
        .await
        .unwrap_err();
    assert!(matches!(err, RuntimeError::CommandPanicked), "{err:?}");
    assert_eq!(handle.with_world(|_w| 3).await.unwrap(), 3);
    assert_eq!(runtime.status(), RuntimeStatus::Running);
    runtime.shutdown().await.unwrap();
}

// ── R17: a dropped shutdown frees its blocking thread ───────────────────────

#[test]
fn r17_a_dropped_shutdown_frees_its_blocking_thread() {
    within(Duration::from_secs(30), || {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let config = manual().with_shutdown_timeout(Duration::from_secs(10));
                let runtime = ready(BevyRuntime::new(config, counting)).await;
                let (gate, blocked) = Gate::hold(&runtime.handle());
                let gate = entered(gate).await;
                // The caller gives up on the shutdown.
                let _ = tokio::time::timeout(Duration::from_millis(50), runtime.shutdown()).await;
                // The one blocking thread is free again soon, not after 10 s.
                let started = Instant::now();
                tokio::task::spawn_blocking(|| ()).await.unwrap();
                assert!(
                    started.elapsed() < Duration::from_secs(3),
                    "{:?}",
                    started.elapsed()
                );
                gate.open();
                blocked.await.unwrap().unwrap();
                runtime.shutdown().await.unwrap();
            });
    });
}
