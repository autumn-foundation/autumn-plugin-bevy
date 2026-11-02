//! The lifecycle state machine. `verification/bevy_runtime.rs` is its Verus
//! model. A change here needs the same change there.

use crate::status::RuntimeStatus;

/// An event that can change the status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Event {
    /// `BevyRuntime::start` spawned the thread.
    Start,
    /// Setup and the first update are done.
    Ready,
    /// A stop request, a closed channel, or `AppExit::Success`.
    Stop,
    /// A panic, a startup timeout, or `AppExit::Error`.
    Fail,
    /// The drain is done. `failed` is `true` after a `Fail`.
    Drained {
        /// The runtime had a failure on the way down.
        failed: bool,
    },
}

impl Event {
    /// All events, for the table tests.
    #[cfg(test)]
    pub(crate) const ALL: [Self; 6] = [
        Self::Start,
        Self::Ready,
        Self::Stop,
        Self::Fail,
        Self::Drained { failed: false },
        Self::Drained { failed: true },
    ];
}

/// The status after `event`, or `None` when the event is not allowed. On
/// `None` the runtime changes nothing.
pub(crate) const fn next(status: RuntimeStatus, event: Event) -> Option<RuntimeStatus> {
    use RuntimeStatus::{Failed, NotStarted, Running, Starting, Stopped, Stopping};
    match (status, event) {
        (NotStarted, Event::Start) => Some(Starting),
        (NotStarted, Event::Stop) => Some(Stopped),
        (Starting, Event::Ready) => Some(Running),
        (Starting | Running, Event::Stop | Event::Fail) => Some(Stopping),
        (Stopping, Event::Drained { failed: false }) => Some(Stopped),
        (Stopping, Event::Drained { failed: true }) => Some(Failed),
        _ => None,
    }
}

/// The rank of a status. Each allowed step makes it go up.
pub(crate) const fn rank(status: RuntimeStatus) -> u8 {
    match status {
        RuntimeStatus::NotStarted => 0,
        RuntimeStatus::Starting => 1,
        RuntimeStatus::Running => 2,
        RuntimeStatus::Stopping => 3,
        RuntimeStatus::Stopped | RuntimeStatus::Failed => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use RuntimeStatus::{Failed, NotStarted, Running, Starting, Stopped, Stopping};
    use proptest::prelude::*;

    /// The `next` function of `verification/bevy_runtime.rs`, row by row.
    /// Pairs that are not in this table are refused.
    const SPEC_TABLE: [(RuntimeStatus, Event, RuntimeStatus); 9] = [
        (NotStarted, Event::Start, Starting),
        (NotStarted, Event::Stop, Stopped),
        (Starting, Event::Ready, Running),
        (Starting, Event::Stop, Stopping),
        (Starting, Event::Fail, Stopping),
        (Running, Event::Stop, Stopping),
        (Running, Event::Fail, Stopping),
        (Stopping, Event::Drained { failed: false }, Stopped),
        (Stopping, Event::Drained { failed: true }, Failed),
    ];

    fn spec(status: RuntimeStatus, event: Event) -> Option<RuntimeStatus> {
        SPEC_TABLE
            .iter()
            .find(|(s, e, _)| *s == status && *e == event)
            .map(|(_, _, to)| *to)
    }

    #[test]
    fn lifecycle_matches_spec_table() {
        for status in RuntimeStatus::ALL {
            for event in Event::ALL {
                assert_eq!(
                    next(status, event),
                    spec(status, event),
                    "{status:?} + {event:?}"
                );
            }
        }
    }

    #[test]
    fn terminal_states_are_absorbing() {
        for status in [Stopped, Failed] {
            for event in Event::ALL {
                assert_eq!(next(status, event), None, "{status:?} + {event:?}");
            }
        }
    }

    #[test]
    fn rank_never_decreases() {
        for status in RuntimeStatus::ALL {
            for event in Event::ALL {
                if let Some(to) = next(status, event) {
                    assert!(rank(to) > rank(status), "{status:?} -> {to:?}");
                }
            }
        }
    }

    #[test]
    fn rank_order_matches_spec() {
        let ranks: Vec<u8> = RuntimeStatus::ALL.into_iter().map(rank).collect();
        assert_eq!(ranks, [0, 1, 2, 3, 4, 4]);
    }

    fn any_event() -> impl Strategy<Value = Event> {
        prop::sample::select(Event::ALL.to_vec())
    }

    proptest! {
        /// Random event sequences keep the spec invariants: the rank goes
        /// up, a terminal status stays, Running comes only after Starting,
        /// and Failed comes only after a failed drain.
        #[test]
        fn lifecycle_proptest(events in prop::collection::vec(any_event(), 0..40)) {
            let mut status = NotStarted;
            let mut seen_starting = false;
            for event in events {
                let before = status;
                if let Some(to) = next(status, event) {
                    prop_assert!(rank(to) > rank(before));
                    prop_assert!(!before.is_terminal());
                    if to == Running {
                        prop_assert!(seen_starting && before == Starting);
                    }
                    if to == Failed {
                        prop_assert_eq!(event, Event::Drained { failed: true });
                    }
                    status = to;
                }
                seen_starting |= status == Starting;
            }
        }
    }
}
