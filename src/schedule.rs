//! The fixed-rate tick schedule. `verification/bevy_runtime.rs` proves its
//! rules S1 to S3.

use std::time::{Duration, Instant};

/// The next deadline and the count of skipped ticks, after the tick that was
/// due at `prev` ends at `now`.
///
/// On time, the next deadline is `prev + period`, so the phase does not
/// drift. When late, the schedule skips the missed ticks and the next
/// deadline is `now + period`, so there is no burst.
pub(crate) fn next_deadline(prev: Instant, period: Duration, now: Instant) -> (Instant, u64) {
    let on_time = prev + period;
    if now < on_time {
        return (on_time, 0);
    }
    let late = now.saturating_duration_since(prev).as_nanos();
    let skipped = late / period.as_nanos().max(1);
    (now + period, u64::try_from(skipped).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const PERIOD: Duration = Duration::from_millis(10);

    #[test]
    fn next_deadline_table() {
        let base = Instant::now();
        let ms = Duration::from_millis;
        // (now - prev, expected next - prev, expected skipped)
        let rows = [
            (ms(0), ms(10), 0),
            (ms(3), ms(10), 0),
            (ms(9), ms(10), 0),
            (ms(10), ms(20), 1),
            (ms(15), ms(25), 1),
            (ms(100), ms(110), 10),
        ];
        for (late, next_after, skipped) in rows {
            let (next, got_skipped) = next_deadline(base, PERIOD, base + late);
            assert_eq!(next - base, next_after, "late by {late:?}");
            assert_eq!(got_skipped, skipped, "late by {late:?}");
        }
    }

    proptest! {
        /// S1, S2 and S3 of the Verus spec on the Rust code.
        #[test]
        fn next_deadline_rules(period_us in 1u64..100_000, late_us in 0u64..10_000_000) {
            let base = Instant::now();
            let period = Duration::from_micros(period_us);
            let now = base + Duration::from_micros(late_us);
            let (next, skipped) = next_deadline(base, period, now);
            prop_assert!(next > now);
            prop_assert!(next >= base + period);
            if now < base + period {
                prop_assert_eq!(next, base + period);
                prop_assert_eq!(skipped, 0);
            } else {
                prop_assert_eq!(next, now + period);
                prop_assert!(skipped >= 1);
                prop_assert_eq!(skipped, late_us / period_us);
            }
        }
    }
}
