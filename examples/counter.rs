//! A counter that a Bevy system increments 10 times each second.
//!
//! ```text
//! cargo run --example counter
//! curl http://localhost:3000/counter
//! curl -X POST http://localhost:3000/reset
//! ```

use autumn_plugin_bevy::prelude::*;
use autumn_web::AutumnResult;
use autumn_web::prelude::*;
use bevy_app::{App, Update};
use bevy_ecs::prelude::*;

/// The count. One update adds one.
#[derive(Resource, Default)]
struct Counter(u64);

fn count(mut counter: ResMut<Counter>) {
    counter.0 += 1;
}

/// Reads the count and the tick number.
#[get("/counter")]
async fn counter(bevy: Bevy) -> AutumnResult<String> {
    let value = bevy
        .with_world(|world| world.resource::<Counter>().0)
        .await?;
    Ok(format!("count={value} ticks={}", bevy.ticks()))
}

/// Sets the count to zero.
#[post("/reset")]
async fn reset(bevy: Bevy) -> AutumnResult<&'static str> {
    bevy.with_world(|world| world.resource_mut::<Counter>().0 = 0)
        .await?;
    Ok("reset")
}

fn game(app: &mut App) {
    app.init_resource::<Counter>().add_systems(Update, count);
}

#[autumn_web::main]
async fn main() {
    let rate = TickRate::new(10).unwrap_or_default();
    let config = BevyConfig::default().with_tick_mode(TickMode::Fixed(rate));
    // The server future is large, so it lives on the heap.
    Box::pin(
        autumn_web::app()
            .plugin(BevyPlugin::new(game).config(config))
            .routes(routes![counter, reset])
            .run(),
    )
    .await;
}
