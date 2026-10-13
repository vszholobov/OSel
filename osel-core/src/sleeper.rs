//! A queue of threads waiting until a deadline
//!
//! `nanosleep`, and the timeouts of `futex` and `epoll_pwait`, block a thread until a time
//! in nanoseconds. The timer wakes the ones whose deadline has passed. The queue keeps the
//! math off the clock, so it is tested on the host with a made-up time.

use crate::sched::ThreadId;
use alloc::vec::Vec;

/// Threads waiting for a deadline, each at most once
#[derive(Debug, Default)]
pub struct Sleepers {
    /// `(deadline nanoseconds, thread)`, not sorted: the queue is short
    entries: Vec<(u64, ThreadId)>,
}

impl Sleepers {
    pub const fn new() -> Sleepers {
        Sleepers { entries: Vec::new() }
    }

    /// Reserves room for `capacity` sleepers, so the timer never allocates while waking
    pub fn reserve(&mut self, capacity: usize) {
        self.entries.reserve(capacity);
    }

    /// Adds `thread` to wake at `deadline`; a second call for the same thread replaces the
    /// first, so a re-check loop never queues it twice
    pub fn add(&mut self, deadline: u64, thread: ThreadId) {
        self.remove(thread);
        self.entries.push((deadline, thread));
    }

    /// Takes `thread` out, e.g. when it was woken for another reason
    pub fn remove(&mut self, thread: ThreadId) {
        self.entries.retain(|&(_, id)| id != thread);
    }

    /// Removes and returns every thread whose deadline is at or before `now`
    pub fn due(&mut self, now: u64) -> Vec<ThreadId> {
        let mut woken = Vec::new();
        self.entries.retain(|&(deadline, id)| {
            if deadline <= now {
                woken.push(id);
                false
            } else {
                true
            }
        });
        woken
    }

    /// The earliest deadline still waiting, if any
    pub fn next_deadline(&self) -> Option<u64> {
        self.entries.iter().map(|&(deadline, _)| deadline).min()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wakes_in_time_order() {
        let mut sleepers = Sleepers::new();
        sleepers.add(30, 3);
        sleepers.add(10, 1);
        sleepers.add(20, 2);
        assert_eq!(sleepers.next_deadline(), Some(10));
        assert!(sleepers.due(5).is_empty(), "nothing due yet");
        assert_eq!(sleepers.due(10), [1], "the earliest only");
        let mut rest = sleepers.due(100);
        rest.sort();
        assert_eq!(rest, [2, 3]);
        assert!(sleepers.is_empty());
    }

    #[test]
    fn re_adding_a_thread_replaces_it() {
        let mut sleepers = Sleepers::new();
        sleepers.add(10, 1);
        sleepers.add(50, 1);
        assert!(sleepers.due(10).is_empty(), "the old deadline is gone");
        assert_eq!(sleepers.due(50), [1]);
    }

    #[test]
    fn removing_cancels_a_wait() {
        let mut sleepers = Sleepers::new();
        sleepers.add(10, 1);
        sleepers.add(10, 2);
        sleepers.remove(1);
        assert_eq!(sleepers.due(10), [2]);
    }
}
