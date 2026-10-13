//! Who runs next, and where kernel stacks go
//!
//! The kernel's scheduler (`src/scheduler.rs`) keeps the threads and switches between them;
//! here is the part that needs no CPU: the policy that picks the next thread and the
//! allocator of kernel stack slots.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

pub type ThreadId = u64;

/// Picks the next thread among the ready ones
pub trait SchedPolicy {
    /// `id` became ready
    fn add(&mut self, id: ThreadId);
    /// `id` is no longer ready, wherever it waits in the queue
    fn remove(&mut self, id: ThreadId);
    /// The thread to run next; it leaves the ready set until `add` again
    fn next(&mut self) -> Option<ThreadId>;
    /// Number of ready threads
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Every ready thread in turn
#[derive(Default)]
pub struct RoundRobin {
    queue: VecDeque<ThreadId>,
}

impl RoundRobin {
    pub fn new() -> RoundRobin {
        RoundRobin { queue: VecDeque::new() }
    }

    /// Room for `threads` ready threads: adding them never allocates, so the queue can be
    /// changed in an interrupt handler
    pub fn with_capacity(threads: usize) -> RoundRobin {
        RoundRobin { queue: VecDeque::with_capacity(threads) }
    }
}

impl SchedPolicy for RoundRobin {
    fn add(&mut self, id: ThreadId) {
        debug_assert!(!self.queue.contains(&id), "thread {id} is ready twice");
        self.queue.push_back(id);
    }

    fn remove(&mut self, id: ThreadId) {
        self.queue.retain(|&queued| queued != id);
    }

    fn next(&mut self) -> Option<ThreadId> {
        self.queue.pop_front()
    }

    fn len(&self) -> usize {
        self.queue.len()
    }
}

/// Fixed slots for kernel stacks in one area of addresses: each slot is a guard page that
/// stays unmapped, then the stack. Only addresses are handed out; mapping is the kernel's.
pub struct StackSlots {
    base: u64,
    stack_size: u64,
    guard_size: u64,
    free: Vec<usize>,
    used: Vec<bool>,
}

/// Where one kernel stack lives
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StackSlot {
    pub index: usize,
    /// First byte of the unmapped guard below the stack
    pub guard: u64,
    /// Lowest byte of the stack
    pub bottom: u64,
    /// One past the highest byte: the stack pointer starts here
    pub top: u64,
}

impl StackSlots {
    /// `count` slots from `base`; `stack_size` and `guard_size` are whole pages
    pub fn new(base: u64, count: usize, stack_size: u64, guard_size: u64) -> StackSlots {
        assert_eq!(base % 4096, 0);
        assert!(stack_size % 4096 == 0 && guard_size % 4096 == 0 && guard_size > 0);
        StackSlots { base, stack_size, guard_size, free: (0..count).rev().collect(), used: alloc::vec![false; count] }
    }

    fn stride(&self) -> u64 {
        self.guard_size + self.stack_size
    }

    /// A free slot, `None` when all are taken
    pub fn allocate(&mut self) -> Option<StackSlot> {
        let index = self.free.pop()?;
        self.used[index] = true;
        Some(self.slot(index))
    }

    /// Gives slot `index` back. Panics if it is not taken: a stack freed twice.
    pub fn free(&mut self, index: usize) {
        assert!(self.used[index], "kernel stack slot {index} freed twice");
        self.used[index] = false;
        self.free.push(index);
    }

    pub fn slot(&self, index: usize) -> StackSlot {
        let guard = self.base + index as u64 * self.stride();
        StackSlot { index, guard, bottom: guard + self.guard_size, top: guard + self.stride() }
    }

    /// The slot whose guard page holds `address`, for a report of a kernel stack overflow
    pub fn guard_of(&self, address: u64) -> Option<usize> {
        let offset = address.checked_sub(self.base)?;
        let index = (offset / self.stride()) as usize;
        (index < self.used.len() && offset % self.stride() < self.guard_size).then_some(index)
    }

    pub fn in_use(&self) -> usize {
        self.used.len() - self.free.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeSet;

    #[test]
    fn round_robin_takes_turns() {
        let mut policy = RoundRobin::new();
        for id in [1, 2, 3] {
            policy.add(id);
        }
        let mut order = Vec::new();
        for _ in 0..6 {
            let id = policy.next().unwrap();
            order.push(id);
            policy.add(id);
        }
        assert_eq!(order, [1, 2, 3, 1, 2, 3]);
        policy.remove(2);
        assert_eq!((policy.next(), policy.next(), policy.next()), (Some(1), Some(3), None));
        assert!(policy.is_empty());
    }

    /// Random adds, picks and removals against a model: a thread that is not ready never
    /// comes out, and every ready one comes out within `len` picks
    #[test]
    fn random_operations_match_a_model() {
        let mut policy = RoundRobin::new();
        let mut ready: BTreeSet<ThreadId> = BTreeSet::new();
        let mut waits: alloc::collections::BTreeMap<ThreadId, usize> = Default::default();
        let mut seed = 0x5c4e_d01e_u64;
        let mut random = move |limit: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % limit
        };
        for _ in 0..10_000 {
            let id = random(8);
            match random(3) {
                0 if !ready.contains(&id) => {
                    policy.add(id);
                    ready.insert(id);
                    waits.insert(id, 0);
                }
                1 => {
                    policy.remove(id);
                    ready.remove(&id);
                }
                _ => match policy.next() {
                    Some(picked) => {
                        assert!(ready.remove(&picked), "{picked} was not ready");
                        for (other, wait) in waits.iter_mut() {
                            if ready.contains(other) {
                                *wait += 1;
                                assert!(*wait <= 8, "{other} starves");
                            }
                        }
                        waits.insert(picked, 0);
                        // most picked threads run again
                        if random(4) != 0 {
                            policy.add(picked);
                            ready.insert(picked);
                        }
                    }
                    None => assert!(ready.is_empty()),
                },
            }
            assert_eq!(policy.len(), ready.len());
        }
    }

    #[test]
    fn stack_slots_have_guards_below() {
        let mut slots = StackSlots::new(0x1000_0000, 3, 0x10000, 0x1000);
        let a = slots.allocate().unwrap();
        assert_eq!(a, StackSlot { index: 0, guard: 0x1000_0000, bottom: 0x1000_1000, top: 0x1001_1000 });
        let b = slots.allocate().unwrap();
        assert_eq!(b.guard, a.top, "the next guard right above the previous stack");
        assert_eq!(slots.guard_of(b.guard + 8), Some(1));
        assert_eq!(slots.guard_of(b.bottom), None, "the stack itself is no guard");
        assert_eq!(slots.guard_of(0x10), None);
        let _c = slots.allocate().unwrap();
        assert!(slots.allocate().is_none(), "all taken");
        slots.free(1);
        assert_eq!(slots.allocate().unwrap().index, 1, "a freed slot is reused");
        assert_eq!(slots.in_use(), 3);
        assert_eq!(slots.guard_of(0x1000_0000 + 3 * 0x11000), None, "past the last slot");
    }

    #[test]
    #[should_panic(expected = "freed twice")]
    fn a_slot_freed_twice_panics() {
        let mut slots = StackSlots::new(0x1000_0000, 2, 0x10000, 0x1000);
        let a = slots.allocate().unwrap();
        slots.free(a.index);
        slots.free(a.index);
    }
}
