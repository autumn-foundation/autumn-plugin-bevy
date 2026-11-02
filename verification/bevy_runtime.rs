use vstd::arithmetic::div_mod::{lemma_div_by_multiple, lemma_div_is_ordered};
use vstd::prelude::*;

verus! {

// The runtime of `autumn-plugin-bevy`: the lifecycle in `src/lifecycle.rs`,
// the command queue in `src/runtime.rs` and the tick scheduler in
// `src/schedule.rs`.
//
// One step here is one critical section in the runtime. The status changes
// under one mutex. The queue is a bounded channel: a count is enough here,
// because the proofs are about counts, not about order. The tests
// `lifecycle_matches_spec_table` and `lifecycle_proptest` check the same rules
// on the Rust code.

// ── Status ─────────────────────────────────────────────────────────────────

pub open spec fn not_started() -> int { 0 }
pub open spec fn starting() -> int { 1 }
pub open spec fn running() -> int { 2 }
pub open spec fn stopping() -> int { 3 }
pub open spec fn stopped() -> int { 4 }
pub open spec fn failed() -> int { 5 }

pub open spec fn valid(s: int) -> bool {
    0 <= s <= 5
}

pub open spec fn terminal(s: int) -> bool {
    s == stopped() || s == failed()
}

/// Stopped and Failed have the same rank: both are the end.
pub open spec fn rank(s: int) -> int {
    if s == failed() { 4 } else { s }
}

// ── Events ─────────────────────────────────────────────────────────────────

pub open spec fn ev_start() -> int { 0 }
pub open spec fn ev_ready() -> int { 1 }
pub open spec fn ev_stop() -> int { 2 }
pub open spec fn ev_fail() -> int { 3 }
pub open spec fn ev_drained_ok() -> int { 4 }
pub open spec fn ev_drained_failed() -> int { 5 }

/// No change.
pub open spec fn refused() -> int { -1 }

/// `lifecycle::next`. `refused()` means the event is not allowed.
pub open spec fn next(s: int, e: int) -> int {
    if s == not_started() && e == ev_start() {
        starting()
    } else if s == not_started() && e == ev_stop() {
        stopped()
    } else if s == starting() && e == ev_ready() {
        running()
    } else if (s == starting() || s == running()) && (e == ev_stop() || e == ev_fail()) {
        stopping()
    } else if s == stopping() && e == ev_drained_ok() {
        stopped()
    } else if s == stopping() && e == ev_drained_failed() {
        failed()
    } else {
        refused()
    }
}

/// I1: a terminal status refuses every event.
pub proof fn terminal_is_absorbing(s: int, e: int)
    requires
        terminal(s),
    ensures
        next(s, e) == refused(),
{}

/// A step from a valid status gives a valid status.
pub proof fn next_is_valid(s: int, e: int)
    requires
        valid(s),
        next(s, e) != refused(),
    ensures
        valid(next(s, e)),
{}

/// I2: each allowed step makes the rank go up. Thus no status comes back.
pub proof fn rank_goes_up(s: int, e: int)
    requires
        valid(s),
        next(s, e) != refused(),
    ensures
        rank(next(s, e)) > rank(s),
{}

/// Running comes only from Starting, by Ready.
pub proof fn running_only_after_ready(s: int, e: int)
    ensures
        next(s, e) == running() ==> s == starting() && e == ev_ready(),
{}

/// Failed comes only from Stopping, by a failed drain. Stopped comes from
/// Stopping or, with no thread, from NotStarted.
pub proof fn terminal_only_after_drain(s: int, e: int)
    ensures
        next(s, e) == failed() ==> s == stopping() && e == ev_drained_failed(),
        next(s, e) == stopped() ==> (s == stopping() && e == ev_drained_ok()) || (s
            == not_started() && e == ev_stop()),
{}

// ── Runtime model ──────────────────────────────────────────────────────────

pub struct Rt {
    pub status: int,
    /// Commands in the channel.
    pub queued: nat,
    /// Commands that the channel took.
    pub accepted: nat,
    /// Commands that got a reply (a result, an error, or a dropped reply).
    pub replied: nat,
    pub cap: nat,
    pub ticks: nat,
    pub failed: bool,
    /// The tick deadline has passed.
    pub due: bool,
}

/// The channel is open from the start until the drain closes it.
pub open spec fn channel_open(s: int) -> bool {
    s == starting() || s == running() || s == stopping()
}

pub open spec fn inv(r: Rt) -> bool {
    &&& valid(r.status)
    &&& r.cap > 0
    // I3: the queue is bounded.
    &&& r.queued <= r.cap
    // I5: each accepted command is queued or has its reply.
    &&& r.accepted == r.replied + r.queued
    // I6: when the channel is closed, no command waits.
    &&& (!channel_open(r.status) ==> r.queued == 0)
    // The failed flag is set only on the way down, and it decides the end.
    &&& (r.failed ==> r.status >= stopping())
    &&& (r.status == failed() ==> r.failed)
    &&& (r.status == stopped() ==> !r.failed)
    &&& (r.due ==> r.status == running())
}

pub open spec fn init(cap: nat) -> Rt {
    Rt {
        status: not_started(),
        queued: 0,
        accepted: 0,
        replied: 0,
        cap,
        ticks: 0,
        failed: false,
        due: false,
    }
}

pub proof fn init_holds(cap: nat)
    requires
        cap > 0,
    ensures
        inv(init(cap)),
{}

// Each step is `(enabled, after)`. When a step is not enabled, the runtime
// refuses it and changes nothing.

/// `BevyRuntime::start`.
pub open spec fn start(r: Rt) -> Rt {
    Rt { status: starting(), ..r }
}

/// The boot update is done: tick 1, then Running.
pub open spec fn ready(r: Rt) -> Rt {
    Rt { status: running(), ticks: r.ticks + 1, ..r }
}

/// `BevyHandle::with_world`: the result, and whether the channel took the
/// command. I4: it takes a command only while the channel is open and has
/// room.
pub open spec fn submit(r: Rt) -> (Rt, bool) {
    if channel_open(r.status) && r.queued < r.cap {
        (Rt { queued: r.queued + 1, accepted: r.accepted + 1, ..r }, true)
    } else {
        (r, false)
    }
}

/// The loop runs one command, or skips one whose caller has gone. I7 and I9:
/// only in Running, and only when no tick is due.
pub open spec fn exec_enabled(r: Rt) -> bool {
    r.status == running() && r.queued > 0 && !r.due
}

pub open spec fn exec_one(r: Rt) -> Rt {
    Rt { queued: (r.queued - 1) as nat, replied: r.replied + 1, ..r }
}

/// The deadline passes.
pub open spec fn become_due(r: Rt) -> Rt {
    Rt { due: true, ..r }
}

/// One update. I7: only in Running.
pub open spec fn tick_enabled(r: Rt) -> bool {
    r.status == running()
}

pub open spec fn tick(r: Rt) -> Rt {
    Rt { ticks: r.ticks + 1, due: false, ..r }
}

/// A panic or `AppExit::Error`.
pub open spec fn fail(r: Rt) -> Rt {
    Rt { status: stopping(), failed: true, due: false, ..r }
}

/// A stop request, a closed channel, or `AppExit::Success`.
pub open spec fn stop(r: Rt) -> Rt {
    if r.status == not_started() {
        Rt { status: stopped(), ..r }
    } else {
        Rt { status: stopping(), due: false, ..r }
    }
}

/// The drain rejects one queued command.
pub open spec fn drain_one(r: Rt) -> Rt {
    Rt { queued: (r.queued - 1) as nat, replied: r.replied + 1, ..r }
}

/// The drain is done and the channel is closed.
pub open spec fn finish(r: Rt) -> Rt {
    Rt { status: if r.failed { failed() } else { stopped() }, ..r }
}

/// Every step keeps the invariant, when it is enabled.
pub proof fn steps_keep_the_invariant(r: Rt)
    requires
        inv(r),
    ensures
        r.status == not_started() ==> inv(start(r)),
        r.status == starting() ==> inv(ready(r)),
        inv(submit(r).0),
        exec_enabled(r) ==> inv(exec_one(r)),
        r.status == running() ==> inv(become_due(r)),
        tick_enabled(r) ==> inv(tick(r)),
        (r.status == starting() || r.status == running()) ==> inv(fail(r)),
        (r.status == not_started() || r.status == starting() || r.status == running()) ==> inv(
            stop(r),
        ),
        (r.status == stopping() && r.queued > 0) ==> inv(drain_one(r)),
        (r.status == stopping() && r.queued == 0) ==> inv(finish(r)),
{}

/// I4: a refused submit changes nothing, and it is refused only when the
/// channel is closed or full.
pub proof fn refused_submit_changes_nothing(r: Rt)
    requires
        inv(r),
    ensures
        !submit(r).1 ==> submit(r).0 == r && (!channel_open(r.status) || r.queued == r.cap),
{}

/// I6: at the end, every accepted command has its reply.
pub proof fn no_command_is_lost(r: Rt)
    requires
        inv(r),
        terminal(r.status),
    ensures
        r.accepted == r.replied,
{}

/// I8: a tick adds one to the count, and nothing makes the count go down.
pub proof fn ticks_never_go_down(r: Rt)
    requires
        inv(r),
    ensures
        tick(r).ticks == r.ticks + 1,
        ready(r).ticks == r.ticks + 1,
        exec_one(r).ticks == r.ticks,
        fail(r).ticks == r.ticks,
        stop(r).ticks == r.ticks,
        drain_one(r).ticks == r.ticks,
        finish(r).ticks == r.ticks,
{}

/// I10: after a failure, no update and no command runs.
pub proof fn no_work_after_failure(r: Rt)
    requires
        inv(r),
        r.failed,
    ensures
        !tick_enabled(r),
        !exec_enabled(r),
        r.status == stopping() ==> finish(r).status == failed(),
{}

/// I11: the health check is up only in Running.
pub open spec fn health_up(r: Rt, busy_ms: nat, threshold_ms: nat) -> bool {
    r.status == running() && busy_ms < threshold_ms
}

pub proof fn health_up_means_running(r: Rt, busy_ms: nat, threshold_ms: nat)
    ensures
        health_up(r, busy_ms, threshold_ms) ==> r.status == running(),
{}

// ── Tick scheduler (`schedule::next_deadline`), in nanoseconds ─────────────

/// The next deadline and the count of skipped ticks, after the tick that was
/// due at `prev` ends at `now`.
pub open spec fn next_deadline(prev: int, period: int, now: int) -> (int, int) {
    if now < prev + period {
        (prev + period, 0)
    } else {
        (now + period, (now - prev) / period)
    }
}

/// S1: the next deadline is in the future.
pub proof fn deadline_is_in_the_future(prev: int, period: int, now: int)
    requires
        period > 0,
    ensures
        next_deadline(prev, period, now).0 > now,
{}

/// S2: deadlines go up by at least one period.
pub proof fn deadlines_go_up(prev: int, period: int, now: int)
    requires
        period > 0,
    ensures
        next_deadline(prev, period, now).0 >= prev + period,
{}

/// S3: on time, the schedule keeps its phase and skips nothing. When late,
/// it skips the missed ticks and runs no burst.
pub proof fn late_ticks_are_skipped(prev: int, period: int, now: int)
    requires
        period > 0,
        now >= prev,
    ensures
        now < prev + period ==> next_deadline(prev, period, now) == (prev + period, 0int),
        now >= prev + period ==> next_deadline(prev, period, now).0 == now + period
            && next_deadline(prev, period, now).1 >= 1,
{
    if now >= prev + period {
        lemma_div_is_ordered(period, now - prev, period);
        lemma_div_by_multiple(1, period);
        assert(1 * period == period);
        assert(period / period == 1);
    }
}

/// S4: one wake runs at most one update. The loop runs an update only when
/// `due`, and the update clears `due`.
pub proof fn one_update_per_due_deadline(r: Rt)
    requires
        inv(r),
        tick_enabled(r),
    ensures
        !tick(r).due,
{}

fn main() {}

} // verus!
