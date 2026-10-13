//! futex module: the `futex` system call Go uses to park and wake its threads
//!
//! Only the two operations Go needs, both private to one process: `FUTEX_WAIT` sleeps while
//! the word at an address still holds an expected value, `FUTEX_WAKE` wakes waiters. The
//! wait queue is `osel_core::futex`; a timeout goes through the sleeper queue of `time`.

use crate::scheduler;
use crate::sync::IrqSpinlock;
use crate::time;
use osel_core::errno::{Errno, KResult};
use osel_core::futex::{FutexKey, Futexes};
use x86_64::instructions::interrupts as irq;

static FUTEXES: IrqSpinlock<Futexes> = IrqSpinlock::new("FUTEXES", Futexes::new());

/// Reserves room for up to `max_waiters` parked threads, so waking never allocates
pub fn init(max_waiters: usize) {
    FUTEXES.lock().reserve(max_waiters);
}

/// Drops every futex wait of a thread being torn down
pub fn forget(thread: osel_core::sched::ThreadId) {
    FUTEXES.lock().forget_thread(thread);
}

const FUTEX_WAIT: u64 = 0;
const FUTEX_WAKE: u64 = 1;
/// Flags on top of the operation: private to the process, and a realtime timeout clock
const FUTEX_PRIVATE_FLAG: u64 = 128;
const FUTEX_CLOCK_REALTIME: u64 = 256;

/// `futex(uaddr, op, val, timeout, ...)`. `timeout`, for `FUTEX_WAIT`, is a relative
/// `timespec`; `reader` copies the word at `uaddr` and checks the address belongs to the
/// program.
pub fn futex(
    space: u64,
    uaddr: u64,
    op: u64,
    val: u32,
    timeout: Option<u64>,
    mut reader: impl FnMut(u64) -> KResult<u32>,
) -> KResult<u64> {
    let key = FutexKey { space, address: uaddr };
    match op & !(FUTEX_PRIVATE_FLAG | FUTEX_CLOCK_REALTIME) {
        FUTEX_WAIT => {
            let deadline = timeout.map(|relative| time::monotonic_ns().saturating_add(relative));
            wait(key, val, deadline, &mut reader)
        }
        FUTEX_WAKE => Ok(wake(key, val as usize) as u64),
        _ => Err(Errno::ENOSYS),
    }
}

/// Sleeps while the word at the key still equals `val`. Returns 0 when woken, `EAGAIN` if
/// the value already differs, `ETIMEDOUT` at the deadline, `EINTR` on a kill.
fn wait(key: FutexKey, val: u32, deadline: Option<u64>, reader: &mut impl FnMut(u64) -> KResult<u32>) -> KResult<u64> {
    if scheduler::current_killed() {
        return Err(Errno::EINTR);
    }
    // interrupts off so no waker runs between the value check and the enqueue; one CPU, so
    // this is enough. The read goes through the physical map, not a fault, so holding no
    // futex lock yet is fine.
    irq::disable();
    let result = (|| {
        if reader(key.address)? != val {
            return Err(Errno::EAGAIN);
        }
        if deadline.is_some_and(|deadline| time::monotonic_ns() >= deadline) {
            return Err(Errno::ETIMEDOUT);
        }
        let id = scheduler::current_id();
        FUTEXES.lock().wait(key, id);
        // blocks until woken by `wake`, the timer at the deadline, or a kill
        time::wait_until(deadline, id);
        FUTEXES.lock().cancel(key, id);
        if scheduler::current_killed() {
            return Err(Errno::EINTR);
        }
        if deadline.is_some_and(|deadline| time::monotonic_ns() >= deadline) {
            return Err(Errno::ETIMEDOUT);
        }
        // a real or spurious wake: Go rechecks its own condition
        Ok(0)
    })();
    irq::enable();
    result
}

/// Wakes up to `count` waiters on the key, returns how many
fn wake(key: FutexKey, count: usize) -> usize {
    let woken = FUTEXES.lock().wake(key, count);
    let n = woken.len();
    for id in woken {
        scheduler::wake(id);
    }
    n
}
