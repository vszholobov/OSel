//! time module: the monotonic clock, the real-time clock and sleeping until a deadline
//!
//! The clock is the PIT tick counter (`interrupts::ticks`, `TICK_HZ` a second) turned into
//! nanoseconds by `osel_core::clock`. The real-time clock adds a base read from the CMOS at
//! boot. `nanosleep` and the timeouts of `futex` and `epoll` block a thread on the
//! `SLEEPERS` queue; the timer wakes the ones whose deadline passed.

use crate::interrupts::{self, TICK_HZ};
use crate::scheduler;
use crate::sync::IrqSpinlock;
use osel_core::clock;
use osel_core::errno::{Errno, KResult};
use osel_core::sched::ThreadId;
use osel_core::sleeper::Sleepers;
use x86_64::instructions::interrupts as irq;

static SLEEPERS: IrqSpinlock<Sleepers> = IrqSpinlock::new("SLEEPERS", Sleepers::new());
/// Real time (nanoseconds since the Unix epoch) at the moment the monotonic clock read 0,
/// from the CMOS at boot
static REALTIME_BASE_NS: IrqSpinlock<u64> = IrqSpinlock::new("REALTIME_BASE", 0);

/// Linux clock ids a program may ask for
pub const CLOCK_REALTIME: u64 = 0;
pub const CLOCK_MONOTONIC: u64 = 1;
const CLOCK_MONOTONIC_RAW: u64 = 4;
const CLOCK_REALTIME_COARSE: u64 = 5;
const CLOCK_MONOTONIC_COARSE: u64 = 6;
const CLOCK_BOOTTIME: u64 = 7;

/// Reads the CMOS real-time clock once, sets the real-time base and reserves room in the
/// sleeper queue so the timer never allocates while waking a thread
pub fn init(max_sleepers: usize) {
    SLEEPERS.lock().reserve(max_sleepers);
    *REALTIME_BASE_NS.lock() = interrupts::wall_clock_unix_nanos().saturating_sub(monotonic_ns());
}

/// Nanoseconds since boot
pub fn monotonic_ns() -> u64 {
    clock::ticks_to_nanos(interrupts::ticks(), TICK_HZ)
}

/// Nanoseconds since the Unix epoch
pub fn realtime_ns() -> u64 {
    REALTIME_BASE_NS.lock().saturating_add(monotonic_ns())
}

/// The time of `clock_id`, `None` for an unknown clock (`EINVAL`)
pub fn clock_ns(clock_id: u64) -> Option<u64> {
    match clock_id {
        CLOCK_MONOTONIC | CLOCK_MONOTONIC_RAW | CLOCK_MONOTONIC_COARSE | CLOCK_BOOTTIME => Some(monotonic_ns()),
        CLOCK_REALTIME | CLOCK_REALTIME_COARSE => Some(realtime_ns()),
        _ => None,
    }
}

/// Blocks the running thread until the monotonic clock reaches `deadline_ns`. Returns early
/// with `EINTR` if the process is killed. One tick (10 ms) is the finest real wait.
pub fn sleep_until(deadline_ns: u64) -> KResult<()> {
    loop {
        if scheduler::current_killed() {
            return Err(Errno::EINTR);
        }
        // checked and queued with interrupts off: the timer can't wake us in between
        irq::disable();
        if monotonic_ns() >= deadline_ns {
            irq::enable();
            return Ok(());
        }
        SLEEPERS.lock().add(deadline_ns, scheduler::current_id());
        scheduler::block_current();
        irq::enable();
    }
}

/// Queues the running thread to be woken at `deadline_ns` and blocks it, like `sleep_until`
/// but the caller re-checks its own condition (a `futex` value, an `epoll` event) on wake.
/// Interrupts must be off; they stay off.
pub fn wait_until(deadline_ns: Option<u64>, thread: ThreadId) {
    if let Some(deadline) = deadline_ns {
        SLEEPERS.lock().add(deadline, thread);
    }
    scheduler::block_current();
    SLEEPERS.lock().remove(thread);
}

/// Drops a sleep of a thread being torn down
pub fn forget(thread: ThreadId) {
    SLEEPERS.lock().remove(thread);
}

/// Wakes every thread whose sleep deadline has passed. Called by the timer with interrupts
/// off, so a thread that just queued itself is not missed.
pub fn wake_due() {
    let now = monotonic_ns();
    let woken = SLEEPERS.lock().due(now);
    for id in woken {
        scheduler::wake(id);
    }
}
