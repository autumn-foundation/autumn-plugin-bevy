//! `BevyRuntime` and `BevyHandle` behavior, with no Autumn app.

use std::num::NonZero;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use autumn_plugin_bevy::prelude::*;
use autumn_plugin_bevy::{BevyConfig, QueueCapacity, TickMode, TickRate};
use bevy_app::{App, AppExit, Last, Plugin, Startup, Update};
use bevy_ecs::prelude::*;

use crate::support::{
    Gate, Log, Updates, Value, count_updates, counting, current_thread, entered, eventually,
    manual, ready,
};

fn thread_name() -> String {
    std::thread::current().name().unwrap_or("").to_owned()
}

// ── AC-02 ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn unstarted_runtime_rejects_calls_and_never_runs_setup() {
    let runs = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&runs);
    let runtime = BevyRuntime::new(manual(), move |_app: &mut App| {
        seen.fetch_add(1, Ordering::SeqCst);
    });
    let handle = runtime.handle();

    let started = Instant::now();
    let err = handle.with_world(|w| w.entity_count()).await.unwrap_err();
    assert!(
        started.elapsed() < Duration::from_millis(10),
        "{:?}",
        started.elapsed()
    );
    assert!(
        matches!(err, RuntimeError::NotRunning(RuntimeStatus::NotStarted)),
        "{err:?}"
    );
    assert!(matches!(
        runtime.wait_ready().await,
        Err(RuntimeError::NotRunning(RuntimeStatus::NotStarted))
    ));

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    assert_eq!(runtime.status(), RuntimeStatus::NotStarted);
}

#[tokio::test]
async fn shutdown_before_start_stops_without_a_thread() {
    let runs = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&runs);
    let runtime = BevyRuntime::new(manual(), move |_app: &mut App| {
        seen.fetch_add(1, Ordering::SeqCst);
    });
    runtime.shutdown().await.expect("shutdown");
    assert_eq!(runtime.status(), RuntimeStatus::Stopped);
    assert!(matches!(
        runtime.start(),
        Err(RuntimeError::NotRunning(RuntimeStatus::Stopped))
    ));
    assert_eq!(runs.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn start_twice_is_already_started() {
    let runtime = ready(BevyRuntime::new(manual(), counting)).await;
    assert!(matches!(runtime.start(), Err(RuntimeError::AlreadyStarted)));
    runtime.shutdown().await.expect("shutdown");
}

// ── AC-03 ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn all_bevy_work_runs_on_named_thread() {
    let names = Arc::new(Mutex::new(Vec::<(String, std::thread::ThreadId)>::new()));
    let in_setup = Arc::clone(&names);
    let in_system = Arc::clone(&names);
    let runtime = ready(BevyRuntime::new(manual(), move |app: &mut App| {
        in_setup
            .lock()
            .unwrap()
            .push((thread_name(), std::thread::current().id()));
        app.add_systems(Update, move || {
            in_system
                .lock()
                .unwrap()
                .push((thread_name(), std::thread::current().id()));
        });
    }))
    .await;

    let handle = runtime.handle();
    let in_command = handle
        .with_world(|_w| (thread_name(), std::thread::current().id()))
        .await
        .unwrap();
    names.lock().unwrap().push(in_command);

    let caller = std::thread::current().id();
    let names = names.lock().unwrap().clone();
    assert!(
        names.len() >= 3,
        "setup, one update and one command: {names:?}"
    );
    let first = names[0].1;
    for (name, id) in &names {
        assert_eq!(name, "autumn-bevy");
        assert_eq!(*id, first, "one thread for all Bevy work");
        assert_ne!(*id, caller);
    }
    runtime.shutdown().await.unwrap();
}

// ── AC-05 ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn commands_run_in_fifo_order() {
    let runtime = ready(BevyRuntime::new(manual(), counting)).await;
    let handle = runtime.handle();
    let mut tasks = Vec::new();
    for i in 0..100_u32 {
        // `with_world` puts the command in the queue on its first poll, so
        // polling each call in turn fixes the order.
        let call = handle.with_world(move |w| w.resource_mut::<Log>().0.push(i));
        let mut call = Box::pin(call);
        let first = futures_poll_once(&mut call).await;
        tasks.push((call, first));
    }
    for (call, first) in tasks {
        if first.is_none() {
            call.await.unwrap();
        }
    }
    let log = handle
        .with_world(|w| w.resource::<Log>().0.clone())
        .await
        .unwrap();
    assert_eq!(log, (0..100).collect::<Vec<_>>());
    runtime.shutdown().await.unwrap();
}

/// Polls a future one time. Returns its output if it is ready.
async fn futures_poll_once<F: std::future::Future + Unpin>(fut: &mut F) -> Option<F::Output> {
    std::future::poll_fn(|cx| {
        std::task::Poll::Ready(match std::pin::Pin::new(&mut *fut).poll(cx) {
            std::task::Poll::Ready(out) => Some(out),
            std::task::Poll::Pending => None,
        })
    })
    .await
}

// ── AC-08 ──────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_queue_returns_busy_without_waiting() {
    let config = manual().with_queue_capacity(QueueCapacity::new(2).unwrap());
    let runtime = ready(BevyRuntime::new(config, counting)).await;
    let handle = runtime.handle();

    let (gate, blocked) = Gate::hold(&handle);
    let gate = entered(gate).await;

    let mut queued = Vec::new();
    for i in 0..2_u32 {
        let h = handle.clone();
        queued.push(tokio::spawn(async move {
            h.with_world(move |w| w.resource_mut::<Log>().0.push(i))
                .await
        }));
    }
    assert!(eventually(Duration::from_secs(2), || handle.queue_depth() == 2).await);

    let started = Instant::now();
    let err = handle.with_world(|_w| ()).await.unwrap_err();
    assert!(matches!(err, RuntimeError::Busy), "{err:?}");
    assert!(
        started.elapsed() < Duration::from_millis(50),
        "{:?}",
        started.elapsed()
    );

    gate.open();
    blocked.await.unwrap().unwrap();
    for task in queued {
        task.await.unwrap().unwrap();
    }
    runtime.shutdown().await.unwrap();
}

// ── AC-09 ──────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_command_times_out_and_runtime_recovers() {
    let config = manual().with_command_timeout(Some(Duration::from_millis(100)));
    let runtime = ready(BevyRuntime::new(config, counting)).await;
    let handle = runtime.handle();

    let err = handle
        .with_world(|_w| std::thread::sleep(Duration::from_millis(300)))
        .await
        .unwrap_err();
    assert!(
        matches!(err, RuntimeError::Timeout(d) if d == Duration::from_millis(100)),
        "{err:?}"
    );
    assert_eq!(runtime.status(), RuntimeStatus::Running);

    let n = handle
        .with_world_timeout(Some(Duration::from_secs(5)), |w| w.entity_count())
        .await;
    assert!(n.is_ok(), "{n:?}");
    runtime.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timed_out_queued_command_never_runs() {
    let runtime = ready(BevyRuntime::new(manual(), counting)).await;
    let handle = runtime.handle();
    let (gate, blocked) = Gate::hold(&handle);
    let gate = entered(gate).await;

    let ran = Arc::new(AtomicUsize::new(0));
    let flag = Arc::clone(&ran);
    let err = handle
        .with_world_timeout(Some(Duration::from_millis(50)), move |_w| {
            flag.fetch_add(1, Ordering::SeqCst);
        })
        .await
        .unwrap_err();
    assert!(matches!(err, RuntimeError::Timeout(_)), "{err:?}");

    gate.open();
    blocked.await.unwrap().unwrap();
    handle.with_world(|_w| ()).await.unwrap();
    assert_eq!(
        ran.load(Ordering::SeqCst),
        0,
        "a cancelled command must not run"
    );

    let families = autumn_web::actuator::MetricsSource::collect(&runtime.metrics());
    let commands = families
        .iter()
        .find(|f| f.name == "bevy_commands_total")
        .expect("bevy_commands_total");
    let cancelled = commands
        .samples
        .iter()
        .find(|s| {
            s.labels
                .iter()
                .any(|(k, v)| k == "outcome" && v == "cancelled")
        })
        .expect("cancelled sample");
    assert!(
        (cancelled.value - 1.0).abs() < f64::EPSILON,
        "{}",
        cancelled.value
    );
    runtime.shutdown().await.unwrap();
}

// ── AC-10 ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn command_panic_is_contained() {
    let runtime = ready(BevyRuntime::new(manual(), counting)).await;
    let handle = runtime.handle();
    let err = handle
        .with_world(|_w| -> u32 { panic!("command boom") })
        .await
        .unwrap_err();
    assert!(matches!(err, RuntimeError::CommandPanicked), "{err:?}");
    assert_eq!(runtime.status(), RuntimeStatus::Running);
    assert_eq!(handle.with_world(|_w| 7).await.unwrap(), 7);
    runtime.shutdown().await.unwrap();
}

// ── AC-11 ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn update_panic_fails_runtime() {
    let runtime = ready(BevyRuntime::new(manual(), |app: &mut App| {
        counting(app);
        app.add_systems(
            Update,
            (|updates: Res<Updates>| {
                assert!(updates.0 < 3, "update boom on tick 3");
            })
            .after(count_updates),
        );
    }))
    .await;
    let handle = runtime.handle();
    assert_eq!(handle.ticks(), 1);

    let stepped = handle.step(5).await;
    assert!(stepped.is_err(), "{stepped:?}");
    assert!(
        eventually(Duration::from_secs(1), || runtime.status()
            == RuntimeStatus::Failed)
        .await,
        "{:?}",
        runtime.status()
    );
    assert_eq!(handle.ticks(), 3);
    let err = handle.with_world(|_w| ()).await.unwrap_err();
    assert!(
        matches!(err, RuntimeError::NotRunning(RuntimeStatus::Failed)),
        "{err:?}"
    );
}

// ── AC-12 ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn setup_panic_fails_readiness() {
    let runtime = BevyRuntime::new(manual(), |_app: &mut App| panic!("boom in setup"));
    runtime.start().unwrap();
    let err = runtime.wait_ready().await.unwrap_err();
    match err {
        RuntimeError::SetupPanicked(message) => assert!(message.contains("boom"), "{message}"),
        other => panic!("expected SetupPanicked, got {other:?}"),
    }
    assert!(
        eventually(Duration::from_secs(1), || runtime.status()
            == RuntimeStatus::Failed)
        .await
    );
}

// ── AC-13 ──────────────────────────────────────────────────────────────────

struct NeverReady;

impl Plugin for NeverReady {
    fn build(&self, _app: &mut App) {}

    fn ready(&self, _app: &App) -> bool {
        false
    }
}

#[tokio::test]
async fn plugin_never_ready_hits_startup_timeout() {
    let config = manual().with_startup_timeout(Duration::from_millis(200));
    let runtime = BevyRuntime::new(config, |app: &mut App| {
        app.add_plugins(NeverReady);
    });
    runtime.start().unwrap();
    let started = Instant::now();
    let err = runtime.wait_ready().await.unwrap_err();
    assert!(matches!(err, RuntimeError::StartupTimeout(_)), "{err:?}");
    assert!(
        started.elapsed() < Duration::from_millis(700),
        "{:?}",
        started.elapsed()
    );
    assert!(
        eventually(Duration::from_secs(1), || runtime.status()
            == RuntimeStatus::Failed)
        .await
    );
}

// ── AC-14 ──────────────────────────────────────────────────────────────────

fn exit_on_tick(exit: AppExit) -> impl FnOnce(&mut App) + Send + 'static {
    move |app: &mut App| {
        counting(app);
        app.add_systems(
            Update,
            (move |updates: Res<Updates>, mut out: MessageWriter<AppExit>| {
                if updates.0 == 2 {
                    out.write(exit.clone());
                }
            })
            .after(count_updates),
        );
    }
}

#[tokio::test]
async fn app_exit_success_stops_runtime() {
    let runtime = ready(BevyRuntime::new(manual(), exit_on_tick(AppExit::Success))).await;
    let _ = runtime.handle().step(3).await;
    assert!(
        eventually(Duration::from_secs(1), || runtime.status()
            == RuntimeStatus::Stopped)
        .await
    );
    assert_eq!(runtime.handle().ticks(), 2);
}

#[tokio::test]
async fn app_exit_error_fails_runtime() {
    let exit = AppExit::Error(NonZero::new(3).unwrap());
    let runtime = ready(BevyRuntime::new(manual(), exit_on_tick(exit))).await;
    let _ = runtime.handle().step(3).await;
    assert!(
        eventually(Duration::from_secs(1), || runtime.status()
            == RuntimeStatus::Failed)
        .await
    );
}

// ── AC-15 ──────────────────────────────────────────────────────────────────

#[test]
fn reentrant_call_fails_fast() {
    let outcome = current_thread(Duration::from_secs(5), || async {
        let runtime = ready(BevyRuntime::new(manual(), counting)).await;
        let handle = runtime.handle();
        let inner = handle.clone();
        let nested = handle
            .with_world(move |_w| {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .build()
                    .expect("nested runtime");
                (
                    rt.block_on(inner.with_world(|w| w.entity_count())).err(),
                    rt.block_on(inner.step(1)).err(),
                )
            })
            .await
            .expect("outer call");
        runtime.shutdown().await.unwrap();
        nested
    });
    assert!(
        matches!(outcome.0, Some(RuntimeError::Reentrant)),
        "{:?}",
        outcome.0
    );
    assert!(
        matches!(outcome.1, Some(RuntimeError::Reentrant)),
        "{:?}",
        outcome.1
    );
}

#[tokio::test]
async fn a_second_runtime_can_be_called_from_a_command() {
    let other = ready(BevyRuntime::new(manual(), counting)).await;
    let other_handle = other.handle();
    let runtime = ready(BevyRuntime::new(manual(), counting)).await;
    let ok = runtime
        .handle()
        .with_world(move |_w| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("nested runtime");
            rt.block_on(other_handle.with_world(|_w| 5))
        })
        .await
        .unwrap();
    assert_eq!(ok.unwrap(), 5);
    runtime.shutdown().await.unwrap();
    other.shutdown().await.unwrap();
}

// ── AC-16 ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn shutdown_runs_final_update_and_stops() {
    let exits = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&exits);
    let runtime = ready(BevyRuntime::new(manual(), move |app: &mut App| {
        app.add_systems(Last, move |mut reader: MessageReader<AppExit>| {
            seen.fetch_add(reader.read().count(), Ordering::SeqCst);
        });
    }))
    .await;
    let started = Instant::now();
    runtime.shutdown().await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(5) + Duration::from_millis(200));
    assert_eq!(runtime.status(), RuntimeStatus::Stopped);
    assert_eq!(
        exits.load(Ordering::SeqCst),
        1,
        "one final update with AppExit"
    );
    runtime
        .shutdown()
        .await
        .expect("a second shutdown is a no-op");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_times_out_when_thread_is_stuck() {
    let config = manual().with_shutdown_timeout(Duration::from_millis(100));
    let runtime = ready(BevyRuntime::new(config, counting)).await;
    let handle = runtime.handle();
    let stuck = tokio::spawn(async move {
        handle
            .with_world_timeout(None, |_w| std::thread::sleep(Duration::from_millis(1500)))
            .await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let started = Instant::now();
    let err = runtime.shutdown().await.unwrap_err();
    assert!(matches!(err, RuntimeError::ShutdownTimeout(_)), "{err:?}");
    assert!(
        started.elapsed() < Duration::from_millis(300),
        "{:?}",
        started.elapsed()
    );

    stuck.await.unwrap().unwrap();
    assert!(
        eventually(Duration::from_secs(3), || runtime.status()
            == RuntimeStatus::Stopped)
        .await
    );
}

// ── AC-17 ──────────────────────────────────────────────────────────────────

#[derive(Resource)]
struct Boom;

#[derive(Debug, Clone, Copy)]
enum StopPath {
    Shutdown,
    AppExit,
    UpdatePanic,
}

async fn queued_commands_resolve_on(path: StopPath) {
    let config = BevyConfig::default()
        .with_tick_mode(TickMode::Fixed(TickRate::new(100).unwrap()))
        .with_command_timeout(None);
    let runtime = ready(BevyRuntime::new(config, |app: &mut App| {
        counting(app);
        app.add_systems(Update, |boom: Option<Res<Boom>>| {
            assert!(boom.is_none(), "update boom");
        });
    }))
    .await;
    let handle = runtime.handle();
    // The gate command sets up the stop, then holds the thread. The next
    // update after the gate opens stops the runtime, while 8 commands wait.
    let (gate, blocked) = match path {
        StopPath::Shutdown => Gate::hold(&handle),
        StopPath::AppExit => Gate::hold_after(&handle, |w| {
            w.write_message(AppExit::Success);
        }),
        StopPath::UpdatePanic => Gate::hold_after(&handle, |w| w.insert_resource(Boom)),
    };
    let gate = entered(gate).await;

    let mut queued = Vec::new();
    for _ in 0..8 {
        let h = handle.clone();
        queued.push(tokio::spawn(async move { h.with_world(|_w| ()).await }));
    }
    assert!(eventually(Duration::from_secs(2), || handle.queue_depth() == 8).await);

    let stopper = if matches!(path, StopPath::Shutdown) {
        let runtime = Arc::new(runtime);
        let stopper = Arc::clone(&runtime);
        Some((
            runtime,
            tokio::spawn(async move { stopper.shutdown().await }),
        ))
    } else {
        drop(runtime);
        None
    };
    // Let the deadline pass, so the next loop pass runs an update first.
    tokio::time::sleep(Duration::from_millis(30)).await;

    gate.open();
    let started = Instant::now();
    let _ = blocked.await.unwrap();
    for task in queued {
        let result = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap_or_else(|_| panic!("{path:?}: a queued command did not resolve"));
        let _ = result.unwrap();
    }
    assert!(started.elapsed() < Duration::from_secs(1), "{path:?}");
    if let Some((runtime, stopper)) = stopper {
        stopper.await.unwrap().unwrap();
        assert!(runtime.status().is_terminal());
    } else {
        assert!(
            eventually(Duration::from_secs(1), || handle.status().is_terminal()).await,
            "{path:?}: {:?}",
            handle.status()
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_stop_path_answers_queued_commands() {
    for path in [StopPath::Shutdown, StopPath::AppExit, StopPath::UpdatePanic] {
        queued_commands_resolve_on(path).await;
    }
}

struct DropProbe(Arc<Mutex<Option<String>>>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        *self.0.lock().unwrap() = Some(thread_name());
    }
}

#[derive(Resource)]
struct Probe(#[allow(dead_code)] DropProbe);

#[tokio::test]
async fn world_drops_on_bevy_thread_after_last_handle_drop() {
    let dropped_on = Arc::new(Mutex::new(None));
    let probe = Arc::clone(&dropped_on);
    let runtime = ready(BevyRuntime::new(manual(), move |app: &mut App| {
        app.insert_resource(Probe(DropProbe(probe)));
    }))
    .await;
    let handle = runtime.handle();
    let watch = runtime.health();
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        dropped_on.lock().unwrap().is_none(),
        "a live handle keeps the world"
    );
    drop(handle);
    assert!(
        eventually(Duration::from_secs(2), || dropped_on
            .lock()
            .unwrap()
            .is_some())
        .await,
        "the world must drop after the last handle"
    );
    assert_eq!(dropped_on.lock().unwrap().as_deref(), Some("autumn-bevy"));
    drop(watch);
}

// ── AC-18 ──────────────────────────────────────────────────────────────────

#[test]
fn concurrent_calls_do_not_block_reactor() {
    let (done, beats) = current_thread(Duration::from_secs(30), || async {
        let config = BevyConfig::default()
            .with_tick_mode(TickMode::Fixed(TickRate::new(100).unwrap()))
            .with_command_timeout(Some(Duration::from_secs(20)));
        let runtime = ready(BevyRuntime::new(config, |app: &mut App| {
            counting(app);
            app.add_systems(Update, || std::thread::sleep(Duration::from_millis(20)));
        }))
        .await;
        let beats = Arc::new(AtomicUsize::new(0));
        let ticker = Arc::clone(&beats);
        let beat = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(10));
            loop {
                interval.tick().await;
                ticker.fetch_add(1, Ordering::SeqCst);
            }
        });
        let handle = runtime.handle();
        let mut set = tokio::task::JoinSet::new();
        for i in 0..1000_u64 {
            let h = handle.clone();
            set.spawn(async move {
                h.with_world(move |_w| {
                    // Hold the Bevy thread, so the calls are in flight for
                    // seconds.
                    std::thread::sleep(Duration::from_millis(1));
                    i
                })
                .await
            });
        }
        let mut done = 0;
        while let Some(result) = set.join_next().await {
            result.unwrap().unwrap();
            done += 1;
        }
        beat.abort();
        runtime.shutdown().await.unwrap();
        (done, beats.load(Ordering::SeqCst))
    });
    assert_eq!(done, 1000);
    assert!(beats >= 5, "the reactor must keep running: {beats} beats");
}

// ── AC-23 ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn manual_step_is_exact() {
    let runtime = ready(BevyRuntime::new(manual(), counting)).await;
    let handle = runtime.handle();
    assert_eq!(handle.ticks(), 1, "ready means one boot update");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(handle.ticks(), 1, "no update without a step");
    assert_eq!(handle.step(3).await.unwrap(), 4);
    assert_eq!(handle.ticks(), 4);
    let updates = handle
        .with_world(|w| w.resource::<Updates>().0)
        .await
        .unwrap();
    assert_eq!(updates, 4);
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn step_in_fixed_mode_is_wrong_mode() {
    let runtime = ready(BevyRuntime::new(BevyConfig::default(), counting)).await;
    let err = runtime.handle().step(1).await.unwrap_err();
    assert!(matches!(err, RuntimeError::WrongMode), "{err:?}");
    runtime.shutdown().await.unwrap();
}

fn deterministic(app: &mut App) {
    counting(app);
    app.add_systems(Startup, |mut commands: Commands| {
        for i in 0..5 {
            commands.spawn(Value(i));
        }
    });
    app.add_systems(
        Update,
        |mut values: Query<&mut Value>, updates: Res<Updates>| {
            for mut value in &mut values {
                value.0 = value.0.wrapping_mul(31).wrapping_add(updates.0);
            }
        },
    );
}

async fn digest_after(steps: u32) -> u64 {
    let runtime = ready(BevyRuntime::new(manual(), deterministic)).await;
    let handle = runtime.handle();
    handle.step(steps).await.unwrap();
    let mut values = handle
        .with_world(|w| {
            let mut q = w.query::<&Value>();
            q.iter(w).copied().collect::<Vec<_>>()
        })
        .await
        .unwrap();
    values.sort_unstable();
    runtime.shutdown().await.unwrap();
    values.iter().fold(0_u64, |acc, v| {
        acc.wrapping_mul(1_000_003).wrapping_add(v.0)
    })
}

#[tokio::test]
async fn manual_runs_replay_identically() {
    let a = digest_after(10).await;
    let b = digest_after(10).await;
    let c = digest_after(11).await;
    assert_eq!(a, b);
    assert_ne!(a, c);
}

#[tokio::test(start_paused = true)]
async fn paused_time_without_timeout_works() {
    let config = manual().with_command_timeout(None);
    let runtime = ready(BevyRuntime::new(config, counting)).await;
    let handle = runtime.handle();
    handle.step(2).await.unwrap();
    let n = handle
        .with_world(|w| w.resource::<Updates>().0)
        .await
        .unwrap();
    assert_eq!(n, 3);
    runtime.shutdown().await.unwrap();
}

// ── AC-25 ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn fixed_mode_ticks_without_requests() {
    let config = BevyConfig::default().with_tick_mode(TickMode::Fixed(TickRate::new(100).unwrap()));
    let runtime = ready(BevyRuntime::new(config, counting)).await;
    let handle = runtime.handle();
    assert!(
        eventually(Duration::from_secs(2), || handle.ticks() >= 4).await,
        "{}",
        handle.ticks()
    );
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn idle_command_does_not_wait_for_tick() {
    let config = BevyConfig::default().with_tick_mode(TickMode::Fixed(TickRate::new(1).unwrap()));
    let runtime = ready(BevyRuntime::new(config, counting)).await;
    let handle = runtime.handle();
    for _ in 0..3 {
        let started = Instant::now();
        handle.with_world(|w| w.entity_count()).await.unwrap();
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "{:?}",
            started.elapsed()
        );
    }
    runtime.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn command_flood_does_not_stop_ticks() {
    let config = BevyConfig::default().with_tick_mode(TickMode::Fixed(TickRate::new(100).unwrap()));
    let runtime = ready(BevyRuntime::new(config, counting)).await;
    let handle = runtime.handle();
    let before = handle.ticks();
    let stop_at = Instant::now() + Duration::from_millis(500);
    let mut flooders = Vec::new();
    for _ in 0..4 {
        let h = handle.clone();
        flooders.push(tokio::spawn(async move {
            while Instant::now() < stop_at {
                let _ = h.with_world(|w| w.entity_count()).await;
            }
        }));
    }
    for f in flooders {
        f.await.unwrap();
    }
    let gained = handle.ticks() - before;
    assert!(
        gained >= 10,
        "ticks must go on under a command flood: {gained}"
    );
    runtime.shutdown().await.unwrap();
}

// ── AC-27 ──────────────────────────────────────────────────────────────────

fn failing_system() -> bevy_ecs::error::Result {
    Err("this system always fails".into())
}

#[tokio::test]
async fn app_owned_bevy_settings_are_kept() {
    let runtime = ready(BevyRuntime::new(manual(), |app: &mut App| {
        app.set_error_handler(bevy_ecs::error::warn);
        app.add_systems(Update, failing_system);
    }))
    .await;
    let handle = runtime.handle();
    handle.step(10).await.unwrap();
    assert_eq!(runtime.status(), RuntimeStatus::Running);
    assert!(
        bevy_tasks::ComputeTaskPool::try_get().is_none(),
        "the plugin must not add TaskPoolPlugin"
    );
    runtime.shutdown().await.unwrap();
}

#[test]
fn ids_differ_and_debug_names_status() {
    let a = BevyRuntime::new(manual(), counting);
    let b = BevyRuntime::new(manual(), counting);
    assert_ne!(a.id(), b.id());
    assert_eq!(a.handle().runtime_id(), a.id());
    let shown = format!("{a:?}");
    assert!(
        shown.contains("not_started") || shown.contains("NotStarted"),
        "{shown}"
    );
}
