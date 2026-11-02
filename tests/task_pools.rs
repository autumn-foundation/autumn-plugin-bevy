//! The global Bevy task pools. They are global to the process, so this test
//! has its own binary: the other tests check that the plugin never makes
//! them.

#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use autumn_plugin_bevy::{BevyConfig, BevyRuntime, RuntimeStatus};
use bevy_app::{App, Plugin, TaskPoolPlugin};
use bevy_tasks::ComputeTaskPool;

/// Ready only after a task on the compute pool ran. The local executor of
/// the pool runs the task only when the boot loop ticks the pools.
struct ReadyAfterTask(Arc<AtomicBool>);

impl Plugin for ReadyAfterTask {
    fn build(&self, _app: &mut App) {
        let done = Arc::clone(&self.0);
        ComputeTaskPool::get()
            .spawn_local(async move { done.store(true, Ordering::SeqCst) })
            .detach();
    }

    fn ready(&self, _app: &App) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[tokio::test]
async fn boot_ticks_the_task_pools_that_the_app_made() {
    let runtime = BevyRuntime::new(BevyConfig::manual(), |app: &mut App| {
        app.add_plugins(TaskPoolPlugin::default());
        app.add_plugins(ReadyAfterTask(Arc::new(AtomicBool::new(false))));
    });
    runtime.start().unwrap();
    runtime
        .wait_ready()
        .await
        .expect("the pools are ticked while plugins are not ready");
    assert_eq!(runtime.status(), RuntimeStatus::Running);
    runtime.shutdown().await.unwrap();
}
