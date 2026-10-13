//! Wait queues for `futex`, keyed by the address a thread waits on
//!
//! Go parks idle threads in `futex(FUTEX_WAIT)` and wakes them with `FUTEX_WAKE`. The queue
//! here only remembers which threads wait on which address; the value check, the blocking
//! and the timeout live in the kernel. The key carries the address space too, so the same
//! virtual address in two processes does not collide.
//!
//! A flat list, like `sleeper`: Go keeps only a handful of waiters at once, and reserving
//! its capacity up front keeps the timer and waker paths from allocating.

use crate::sched::ThreadId;
use alloc::vec::Vec;

/// What a thread waits on: its address space and the address inside it
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FutexKey {
    pub space: u64,
    pub address: u64,
}

/// Threads waiting on futex addresses, in arrival order (woken oldest first, as Linux)
#[derive(Debug, Default)]
pub struct Futexes {
    waiters: Vec<(FutexKey, ThreadId)>,
}

impl Futexes {
    pub const fn new() -> Futexes {
        Futexes { waiters: Vec::new() }
    }

    /// Reserves room for `capacity` waiters, so waking never allocates
    pub fn reserve(&mut self, capacity: usize) {
        self.waiters.reserve(capacity);
    }

    /// Adds `thread` to the waiters on `key`
    pub fn wait(&mut self, key: FutexKey, thread: ThreadId) {
        self.waiters.push((key, thread));
    }

    /// Removes `thread` from `key`, e.g. after a timeout or a kill
    pub fn cancel(&mut self, key: FutexKey, thread: ThreadId) {
        if let Some(index) = self.waiters.iter().position(|&(k, id)| k == key && id == thread) {
            self.waiters.remove(index);
        }
    }

    /// Removes and returns up to `count` waiters on `key`, oldest first
    pub fn wake(&mut self, key: FutexKey, count: usize) -> Vec<ThreadId> {
        let mut woken = Vec::new();
        let mut index = 0;
        while index < self.waiters.len() && woken.len() < count {
            if self.waiters[index].0 == key {
                woken.push(self.waiters.remove(index).1);
            } else {
                index += 1;
            }
        }
        woken
    }

    pub fn is_empty(&self) -> bool {
        self.waiters.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(address: u64) -> FutexKey {
        FutexKey { space: 1, address }
    }

    #[test]
    fn wakes_oldest_first() {
        let mut futexes = Futexes::new();
        futexes.wait(key(0x1000), 10);
        futexes.wait(key(0x1000), 20);
        futexes.wait(key(0x1000), 30);
        assert_eq!(futexes.wake(key(0x1000), 2), [10, 20]);
        assert_eq!(futexes.wake(key(0x1000), 10), [30], "no more than wait");
        assert!(futexes.is_empty());
        assert!(futexes.wake(key(0x1000), 1).is_empty(), "an empty address wakes nothing");
    }

    #[test]
    fn addresses_and_spaces_are_separate() {
        let mut futexes = Futexes::new();
        futexes.wait(key(0x1000), 1);
        futexes.wait(key(0x2000), 2);
        futexes.wait(FutexKey { space: 2, address: 0x1000 }, 3);
        assert_eq!(futexes.wake(key(0x1000), 10), [1], "only this address and space");
        assert_eq!(futexes.wake(key(0x2000), 10), [2]);
        assert_eq!(futexes.wake(FutexKey { space: 2, address: 0x1000 }, 10), [3]);
    }

    #[test]
    fn cancel_removes_one_waiter() {
        let mut futexes = Futexes::new();
        futexes.wait(key(0x1000), 1);
        futexes.wait(key(0x1000), 2);
        futexes.cancel(key(0x1000), 1);
        assert_eq!(futexes.wake(key(0x1000), 10), [2]);
        futexes.cancel(key(0x2000), 9);
        futexes.cancel(key(0x1000), 2);
        assert!(futexes.is_empty(), "removing the last waiter empties the list");
    }
}
