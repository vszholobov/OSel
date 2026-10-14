//! poll module: `eventfd2`, `epoll_create1`, `epoll_ctl` and `epoll_pwait`
//!
//! The state (an `eventfd` counter, an `epoll` interest list) lives in `osel_core::poll`.
//! Here the descriptors are created and `epoll_pwait` blocks a thread until one of the
//! watched descriptors is ready or a timeout passes. A write to an `eventfd` wakes every
//! thread waiting in `epoll_pwait`, which then polls its descriptors again.

use crate::scheduler;
use crate::sync::IrqSpinlock;
use crate::time;
use alloc::vec::Vec;
use osel_core::clock::NANOS_PER_SEC;
use osel_core::errno::{Errno, KResult};
use osel_core::file::Ready;
use osel_core::sched::ThreadId;
use x86_64::instructions::interrupts as irq;

/// Threads blocked in `epoll_pwait`, woken when an `eventfd` is written
static POLL_WAITERS: IrqSpinlock<Vec<ThreadId>> = IrqSpinlock::new("POLL_WAITERS", Vec::new());

/// `epoll_event` bits Go uses
pub const EPOLLIN: u32 = 0x001;
pub const EPOLLOUT: u32 = 0x004;
/// Size of `struct epoll_event` on x86_64: a `u32` and a packed `u64`
pub const EPOLL_EVENT_SIZE: usize = 12;

/// Reserves room so waking never allocates
pub fn init(max_waiters: usize) {
    POLL_WAITERS.lock().reserve(max_waiters);
}

/// Drops an `epoll_pwait` wait of a thread being torn down
pub fn forget(thread: ThreadId) {
    POLL_WAITERS.lock().retain(|&id| id != thread);
}

/// Wakes every thread in `epoll_pwait`; called after an `eventfd` (any epollable file) is
/// written, so a poller notices the new readiness
pub fn notify() {
    let waiters = POLL_WAITERS.lock().clone();
    for id in waiters {
        scheduler::wake(id);
    }
}

/// Readiness bits of `interest` a file reports ready
pub fn revents(ready: Ready, interest: u32) -> u32 {
    let mut revents = 0;
    if ready.readable {
        revents |= EPOLLIN;
    }
    if ready.writable {
        revents |= EPOLLOUT;
    }
    revents & interest
}

/// `epoll_pwait`: `scan` returns the ready `(data, events)` of the watched descriptors;
/// the loop blocks until it returns some, the timeout passes, or a kill. `timeout_ms`
/// below zero waits forever, zero returns at once. `write_events` copies them to the
/// program and the count is returned.
pub fn epoll_pwait(
    maxevents: usize,
    timeout_ms: i64,
    mut scan: impl FnMut(usize) -> Vec<(u64, u32)>,
    mut write_events: impl FnMut(&[(u64, u32)]) -> KResult<()>,
) -> KResult<u64> {
    let deadline = (timeout_ms >= 0).then(|| time::monotonic_ns().saturating_add(timeout_ms as u64 * (NANOS_PER_SEC / 1000)));
    let id = scheduler::current_id();
    loop {
        if scheduler::current_killed() {
            return Err(Errno::EINTR);
        }
        let events = scan(maxevents);
        if !events.is_empty() {
            write_events(&events)?;
            return Ok(events.len() as u64);
        }
        if timeout_ms == 0 || deadline.is_some_and(|deadline| time::monotonic_ns() >= deadline) {
            return Ok(0);
        }
        // register, then scan once more with interrupts off: an eventfd write (and its
        // `notify`) between the scan above and here would otherwise be missed, hanging an
        // untimed wait forever
        irq::disable();
        if scheduler::current_killed() {
            irq::enable();
            return Err(Errno::EINTR);
        }
        POLL_WAITERS.lock().push(id);
        let events = scan(maxevents);
        if events.is_empty() { time::wait_until(deadline, id); }
        else {
            POLL_WAITERS.lock().retain(|&waiter| waiter != id);
            irq::enable();
            write_events(&events)?;
            return Ok(events.len() as u64);
        }
        POLL_WAITERS.lock().retain(|&waiter| waiter != id);
        irq::enable();
    }
}
