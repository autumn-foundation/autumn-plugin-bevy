//! Messages from the async side to the Bevy thread.

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};

use bevy_ecs::world::World;
use tokio::sync::oneshot;

use crate::error::RuntimeError;

/// One message in the command queue.
pub(crate) enum Msg {
    /// Run a closure on the world.
    Run(Box<dyn Command>),
    /// Run `n` updates (`TickMode::Manual` only).
    Step {
        n: u32,
        reply: oneshot::Sender<Result<u64, RuntimeError>>,
    },
    /// Wake the loop. It then reads the stop flag.
    Wake,
}

/// The result of one command, for the metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Ok,
    Panicked,
    Cancelled,
}

/// A queued closure and its reply channel.
pub(crate) trait Command: Send {
    /// Runs the closure, or skips it when the caller has gone.
    fn run(self: Box<Self>, world: &mut World) -> Outcome;
    /// Sends `err` and does not run the closure.
    fn reject(self: Box<Self>, err: RuntimeError);
}

/// A closure `f` and the channel for its result.
pub(crate) struct Job<F, R> {
    pub(crate) f: F,
    pub(crate) reply: oneshot::Sender<Result<R, RuntimeError>>,
}

impl<F, R> Command for Job<F, R>
where
    F: FnOnce(&mut World) -> R + Send + 'static,
    R: Send + 'static,
{
    fn run(self: Box<Self>, world: &mut World) -> Outcome {
        let Self { f, reply } = *self;
        // A caller that timed out or went away gets no result. Do not change
        // the world for it.
        if reply.is_closed() {
            return Outcome::Cancelled;
        }
        match catch_unwind(AssertUnwindSafe(|| f(world))) {
            Ok(value) => {
                // The caller can go away while the closure runs.
                let _ = reply.send(Ok(value));
                Outcome::Ok
            }
            Err(payload) => {
                tracing::error!(panic = %panic_message(payload.as_ref()), "bevy command panicked");
                let _ = reply.send(Err(RuntimeError::CommandPanicked));
                Outcome::Panicked
            }
        }
    }

    fn reject(self: Box<Self>, err: RuntimeError) {
        let _ = self.reply.send(Err(err));
    }
}

/// The text of a panic payload.
pub(crate) fn panic_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-text panic payload".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_message_reads_str_and_string() {
        let a: Box<dyn Any + Send> = Box::new("boom");
        let b: Box<dyn Any + Send> = Box::new(String::from("bang"));
        let c: Box<dyn Any + Send> = Box::new(7_u8);
        assert_eq!(panic_message(a.as_ref()), "boom");
        assert_eq!(panic_message(b.as_ref()), "bang");
        assert_eq!(panic_message(c.as_ref()), "non-text panic payload");
    }
}
