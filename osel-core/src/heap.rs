//! Kernel heap allocator
//!
//! Small allocations are served from per-size free lists of fixed-size blocks,
//! everything bigger than the largest block goes to a linked list allocator.
//! Free blocks go back to the linked list allocator when it runs out of memory,
//! so many small allocations can't starve big ones forever.

use core::alloc::Layout;
use core::mem;
use core::ptr::{self, NonNull};

/// Block sizes, each one is also its alignment, so they have to be powers of 2
pub const BLOCK_SIZES: &[usize] = &[8, 16, 32, 64, 128, 256, 512, 1024, 2048];

struct ListNode {
    next: *mut ListNode,
}

pub struct FixedSizeBlockAllocator {
    list_heads: [*mut ListNode; BLOCK_SIZES.len()],
    free_blocks: [usize; BLOCK_SIZES.len()],
    fallback_allocator: linked_list_allocator::Heap,
}

// the raw pointers only point into the heap memory owned by the allocator
unsafe impl Send for FixedSizeBlockAllocator {}

/// Index of the smallest block that fits `layout`, `None` for big allocations
pub fn list_index(layout: &Layout) -> Option<usize> {
    let required_block_size = layout.size().max(layout.align());
    BLOCK_SIZES.iter().position(|&s| s >= required_block_size)
}

/// Heap bytes a block really takes: the linked list allocator never hands out less than 16
fn block_cost(size: usize) -> usize {
    size.max(16)
}

fn block_layout(index: usize) -> Layout {
    Layout::from_size_align(BLOCK_SIZES[index], BLOCK_SIZES[index]).unwrap()
}

impl FixedSizeBlockAllocator {
    pub const fn new() -> Self {
        FixedSizeBlockAllocator {
            list_heads: [ptr::null_mut(); BLOCK_SIZES.len()],
            free_blocks: [0; BLOCK_SIZES.len()],
            fallback_allocator: linked_list_allocator::Heap::empty(),
        }
    }

    /// # Safety
    /// The memory range must be valid for reads and writes, unused, and passed only once.
    pub unsafe fn init(&mut self, heap_start: *mut u8, heap_size: usize) {
        assert!(self.fallback_allocator.size() == 0, "heap is already initialized");
        unsafe { self.fallback_allocator.init(heap_start, heap_size) };
    }

    /// Returns a block for `layout` or null when the heap is exhausted
    pub fn allocate(&mut self, layout: Layout) -> *mut u8 {
        let ptr = self.try_allocate(layout);
        // reclaiming can't help a request bigger than all free memory together
        if !ptr.is_null() || self.fallback_free() + self.cached_bytes() < layout.size() {
            return ptr;
        }
        if self.reclaim_free_blocks() == 0 {
            return ptr;
        }
        self.try_allocate(layout)
    }

    /// Bytes held by cached free blocks, as the linked list allocator counts them
    pub fn cached_bytes(&self) -> usize {
        BLOCK_SIZES.iter().zip(self.free_blocks).map(|(&size, count)| block_cost(size) * count).sum()
    }

    /// Bytes handed out and not freed yet, the number to compare for leak checks
    pub fn in_use(&self) -> usize {
        self.size() - self.fallback_free() - self.cached_bytes()
    }

    /// # Safety
    /// `ptr` must come from `allocate` of this allocator with the same `layout`.
    pub unsafe fn deallocate(&mut self, ptr: *mut u8, layout: Layout) {
        match list_index(&layout) {
            Some(index) => {
                // every block is at least as big and aligned as a ListNode
                const _: () = assert!(mem::size_of::<ListNode>() <= 8 && mem::align_of::<ListNode>() <= 8);
                let node = ptr as *mut ListNode;
                unsafe { node.write(ListNode { next: self.list_heads[index] }) };
                self.list_heads[index] = node;
                self.free_blocks[index] += 1;
            }
            None => {
                let ptr = NonNull::new(ptr).expect("deallocating null");
                unsafe { self.fallback_allocator.deallocate(ptr, layout) };
            }
        }
    }

    /// Number of cached free blocks of every size in `BLOCK_SIZES`
    pub fn free_blocks(&self) -> [usize; BLOCK_SIZES.len()] {
        self.free_blocks
    }

    /// Bytes the linked list allocator can still hand out (not counting cached blocks)
    pub fn fallback_free(&self) -> usize {
        self.fallback_allocator.free()
    }

    /// Size of the whole heap
    pub fn size(&self) -> usize {
        self.fallback_allocator.size()
    }

    fn try_allocate(&mut self, layout: Layout) -> *mut u8 {
        match list_index(&layout) {
            Some(index) => {
                let head = self.list_heads[index];
                if head.is_null() {
                    // no free block of this size yet, carve a new one
                    return self.fallback_alloc(block_layout(index));
                }
                self.list_heads[index] = unsafe { (*head).next };
                self.free_blocks[index] -= 1;
                head as *mut u8
            }
            None => self.fallback_alloc(layout),
        }
    }

    fn fallback_alloc(&mut self, layout: Layout) -> *mut u8 {
        match self.fallback_allocator.allocate_first_fit(layout) {
            Ok(ptr) => ptr.as_ptr(),
            Err(()) => ptr::null_mut(),
        }
    }

    /// Gives every cached free block back to the linked list allocator, so they can
    /// merge into bigger holes. Returns the number of released blocks.
    fn reclaim_free_blocks(&mut self) -> usize {
        let mut released = 0;
        for index in 0..BLOCK_SIZES.len() {
            // the linked list allocator walks its address sorted hole list from the lowest
            // hole on every free: blocks given back from the highest address down always
            // land at the front of the list, any other order makes this quadratic
            let mut node = unsafe { sort_descending(self.list_heads[index]) };
            while let Some(current) = NonNull::new(node) {
                node = unsafe { current.as_ref().next };
                unsafe { self.fallback_allocator.deallocate(current.cast(), block_layout(index)) };
                released += 1;
            }
            self.list_heads[index] = ptr::null_mut();
            self.free_blocks[index] = 0;
        }
        released
    }
}

/// Merge sort of a free list by node address, highest first, without allocating
///
/// # Safety
/// `head` must start a valid null terminated list.
unsafe fn sort_descending(head: *mut ListNode) -> *mut ListNode {
    if head.is_null() || unsafe { (*head).next.is_null() } {
        return head;
    }
    // split in the middle: `slow` ends at the last node of the first half
    let (mut slow, mut fast) = (head, unsafe { (*head).next });
    while !fast.is_null() && !unsafe { (*fast).next.is_null() } {
        slow = unsafe { (*slow).next };
        fast = unsafe { (*(*fast).next).next };
    }
    let second = unsafe { (*slow).next };
    unsafe { (*slow).next = ptr::null_mut() };
    let (mut a, mut b) = unsafe { (sort_descending(head), sort_descending(second)) };

    let mut merged: *mut ListNode = ptr::null_mut();
    let mut tail: *mut *mut ListNode = &mut merged;
    while !a.is_null() && !b.is_null() {
        let higher = if a > b { &mut a } else { &mut b };
        unsafe {
            *tail = *higher;
            tail = &mut (**higher).next;
            *higher = (**higher).next;
        }
    }
    unsafe { *tail = if a.is_null() { b } else { a } };
    merged
}

impl Default for FixedSizeBlockAllocator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    const ARENA_SIZE: usize = 64 * 1024;

    struct Arena {
        start: *mut u8,
        size: usize,
        allocator: FixedSizeBlockAllocator,
    }

    impl Arena {
        fn new(size: usize) -> Arena {
            let layout = Layout::from_size_align(size, 4096).unwrap();
            let start = unsafe { std::alloc::alloc(layout) };
            let mut allocator = FixedSizeBlockAllocator::new();
            unsafe { allocator.init(start, size) };
            Arena { start, size, allocator }
        }

        fn contains(&self, ptr: *mut u8, size: usize) -> bool {
            let (start, ptr) = (self.start as usize, ptr as usize);
            ptr >= start && ptr + size <= start + self.size
        }

        fn alloc(&mut self, size: usize, align: usize) -> *mut u8 {
            self.allocator.allocate(Layout::from_size_align(size, align).unwrap())
        }

        fn free(&mut self, ptr: *mut u8, size: usize, align: usize) {
            unsafe { self.allocator.deallocate(ptr, Layout::from_size_align(size, align).unwrap()) }
        }
    }

    impl Drop for Arena {
        fn drop(&mut self) {
            unsafe { std::alloc::dealloc(self.start, Layout::from_size_align(self.size, 4096).unwrap()) };
        }
    }

    fn layout(size: usize, align: usize) -> Layout {
        Layout::from_size_align(size, align).unwrap()
    }

    #[test]
    fn size_classes() {
        assert_eq!(list_index(&layout(0, 1)), Some(0));
        assert_eq!(list_index(&layout(1, 1)), Some(0));
        assert_eq!(list_index(&layout(8, 8)), Some(0));
        assert_eq!(list_index(&layout(9, 1)), Some(1));
        assert_eq!(list_index(&layout(16, 1024)), Some(7), "alignment decides");
        assert_eq!(list_index(&layout(2048, 1)), Some(8));
        assert_eq!(list_index(&layout(2049, 1)), None);
        assert_eq!(list_index(&layout(1, 4096)), None);
    }

    #[test]
    fn blocks_are_aligned_and_inside_the_heap() {
        let mut arena = Arena::new(ARENA_SIZE);
        for (size, align) in [(1, 1), (3, 2), (8, 8), (24, 8), (24, 64), (100, 4), (2048, 2048), (3000, 8), (10, 4096)] {
            let ptr = arena.alloc(size, align);
            assert!(!ptr.is_null(), "{size} {align}");
            assert_eq!(ptr as usize % align, 0, "{size} {align}");
            assert!(arena.contains(ptr, size));
        }
    }

    #[test]
    fn freed_blocks_are_reused_last_in_first_out() {
        let mut arena = Arena::new(ARENA_SIZE);
        let a = arena.alloc(30, 8);
        let b = arena.alloc(30, 8);
        arena.free(a, 30, 8);
        arena.free(b, 30, 8);
        assert_eq!(arena.allocator.free_blocks()[2], 2);
        assert_eq!(arena.alloc(32, 8), b);
        assert_eq!(arena.alloc(17, 1), a, "same size class");
        assert_eq!(arena.allocator.free_blocks()[2], 0);
    }

    #[test]
    fn big_allocations_are_reused() {
        let mut arena = Arena::new(ARENA_SIZE);
        let size = ARENA_SIZE / 2;
        let a = arena.alloc(size, 8);
        assert!(!a.is_null());
        arena.free(a, size, 8);
        let b = arena.alloc(size, 8);
        assert!(!b.is_null(), "the space came back to the fallback allocator");
    }

    #[test]
    fn exhaustion_returns_null() {
        let mut arena = Arena::new(ARENA_SIZE);
        assert!(arena.alloc(ARENA_SIZE * 2, 8).is_null());
        let mut blocks = 0;
        while !arena.alloc(2048, 8).is_null() {
            blocks += 1;
            assert!(blocks <= ARENA_SIZE / 2048);
        }
        assert!(blocks >= ARENA_SIZE / 2048 - 2, "only {blocks} blocks fit");
        assert!(arena.alloc(8, 8).is_null() || arena.allocator.fallback_free() < 64);
    }

    #[test]
    fn small_blocks_do_not_starve_big_allocations() {
        let mut arena = Arena::new(ARENA_SIZE);
        let mut small = Vec::new();
        loop {
            let ptr = arena.alloc(64, 8);
            if ptr.is_null() {
                break;
            }
            small.push(ptr);
        }
        for &ptr in &small {
            arena.free(ptr, 64, 8);
        }
        assert_eq!(arena.allocator.free_blocks()[3], small.len());
        // every byte sits in the 64 byte free list now, the allocation needs them back
        let big = arena.alloc(ARENA_SIZE / 2, 8);
        assert!(!big.is_null(), "free small blocks were not reclaimed");
        assert_eq!(arena.allocator.free_blocks(), [0; BLOCK_SIZES.len()]);
    }

    #[test]
    fn other_size_class_reclaims_too() {
        let mut arena = Arena::new(ARENA_SIZE);
        let mut small = Vec::new();
        while let Some(ptr) = Some(arena.alloc(16, 8)).filter(|p| !p.is_null()) {
            small.push(ptr);
        }
        for &ptr in &small {
            arena.free(ptr, 16, 8);
        }
        assert!(!arena.alloc(1024, 8).is_null());
    }

    #[test]
    fn huge_alignment_goes_to_fallback() {
        let mut arena = Arena::new(ARENA_SIZE);
        let free_before = arena.allocator.fallback_free();
        let ptr = arena.alloc(16, 4096);
        assert_eq!(ptr as usize % 4096, 0);
        arena.free(ptr, 16, 4096);
        assert_eq!(arena.allocator.fallback_free(), free_before);
        assert_eq!(arena.allocator.free_blocks(), [0; BLOCK_SIZES.len()]);
    }

    #[test]
    fn reclaim_of_scattered_blocks_is_fast() {
        let mut arena = Arena::new(1024 * 1024);
        let mut blocks = Vec::new();
        while let Some(ptr) = Some(arena.alloc(8, 8)).filter(|p| !p.is_null()) {
            blocks.push(ptr);
        }
        // every other block in descending address order: the worst case for the hole list
        blocks.sort();
        let freed: Vec<*mut u8> = blocks.iter().copied().step_by(2).rev().collect();
        for &ptr in &freed {
            arena.free(ptr, 8, 8);
        }
        // the freed blocks sit between used ones, so the 64 bytes don't fit even after the
        // reclaim; what matters is that giving back thousands of blocks stays cheap
        let started = std::time::Instant::now();
        assert!(arena.alloc(64, 8).is_null());
        let elapsed = started.elapsed();
        assert_eq!(arena.allocator.free_blocks(), [0; BLOCK_SIZES.len()], "everything was reclaimed");
        assert!(elapsed < std::time::Duration::from_secs(1), "reclaim took {elapsed:?}");
    }

    #[test]
    fn sorting_free_lists() {
        let mut nodes: Vec<ListNode> = (0..37).map(|_| ListNode { next: ptr::null_mut() }).collect();
        let addresses: Vec<*mut ListNode> = nodes.iter_mut().map(|n| n as *mut ListNode).collect();
        // link them in a scrambled order
        let order: Vec<usize> = (0..37).map(|i| (i * 17) % 37).collect();
        for pair in order.windows(2) {
            let (from, to) = (addresses[pair[0]], addresses[pair[1]]);
            unsafe { (*from).next = to };
        }
        let mut node = unsafe { sort_descending(addresses[order[0]]) };
        let mut sorted = Vec::new();
        while !node.is_null() {
            sorted.push(node);
            node = unsafe { (*node).next };
        }
        let mut expected = addresses.clone();
        expected.reverse();
        assert_eq!(sorted, expected, "every node once, highest address first");
        assert!(unsafe { sort_descending(ptr::null_mut()) }.is_null());
    }

    #[test]
    fn eight_byte_blocks_take_sixteen_bytes() {
        // the linked list allocator never hands out less than 16 bytes
        let mut arena = Arena::new(ARENA_SIZE);
        let mut blocks = Vec::new();
        while let Some(ptr) = Some(arena.alloc(8, 8)).filter(|p| !p.is_null()) {
            blocks.push(ptr);
        }
        assert!(blocks.len() <= ARENA_SIZE / 16);
        assert!(blocks.len() >= ARENA_SIZE / 16 - 2);
        for &ptr in &blocks {
            arena.free(ptr, 8, 8);
        }
        assert!(!arena.alloc(ARENA_SIZE - 64, 8).is_null(), "everything comes back");
    }

    #[test]
    fn zero_size_allocations() {
        let mut arena = Arena::new(ARENA_SIZE);
        for align in [1, 8, 2048, 4096] {
            let ptr = arena.alloc(0, align);
            assert!(!ptr.is_null());
            assert_eq!(ptr as usize % align, 0);
            arena.free(ptr, 0, align);
        }
    }

    #[test]
    fn small_size_big_alignment_uses_the_matching_class() {
        let mut arena = Arena::new(ARENA_SIZE);
        let ptr = arena.alloc(1, 2048);
        assert_eq!(ptr as usize % 2048, 0);
        arena.free(ptr, 1, 2048);
        assert_eq!(arena.allocator.free_blocks()[8], 1, "cached as a 2048 byte block");
    }

    #[test]
    fn reclaim_also_helps_small_allocations() {
        let mut arena = Arena::new(ARENA_SIZE);
        let mut big = Vec::new();
        while let Some(ptr) = Some(arena.alloc(2048, 8)).filter(|p| !p.is_null()) {
            big.push(ptr);
        }
        assert!(arena.alloc(8, 8).is_null() || arena.allocator.fallback_free() > 0);
        for &ptr in &big {
            arena.free(ptr, 2048, 8);
        }
        assert!(!arena.alloc(8, 8).is_null());
    }

    #[test]
    fn impossible_requests_keep_the_cache() {
        let mut arena = Arena::new(ARENA_SIZE);
        let blocks: Vec<*mut u8> = (0..100).map(|_| arena.alloc(64, 8)).collect();
        for &ptr in &blocks {
            arena.free(ptr, 64, 8);
        }
        assert!(arena.alloc(ARENA_SIZE * 2, 8).is_null());
        assert_eq!(arena.allocator.free_blocks()[3], 100, "the cache was flushed for nothing");
    }

    #[test]
    fn usage_accounting() {
        let mut arena = Arena::new(ARENA_SIZE);
        assert_eq!(arena.allocator.in_use(), 0);
        let small = arena.alloc(8, 8);
        let big = arena.alloc(5000, 8);
        assert_eq!(arena.allocator.in_use(), 16 + 5000);
        arena.free(small, 8, 8);
        assert_eq!(arena.allocator.cached_bytes(), 16);
        arena.free(big, 5000, 8);
        assert_eq!(arena.allocator.in_use(), 0, "nothing leaks after freeing everything");
    }

    #[test]
    #[should_panic(expected = "already initialized")]
    fn double_init_is_refused() {
        let mut arena = Arena::new(ARENA_SIZE);
        let start = arena.start;
        unsafe { arena.allocator.init(start, 4096) };
    }

    /// Pseudo random allocations and frees: live blocks never overlap and keep their contents
    #[test]
    fn random_workload_keeps_data_intact() {
        let mut arena = Arena::new(256 * 1024);
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut random = move |limit: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % limit) as usize
        };
        // (pointer, size, align, fill byte)
        let mut live: Vec<(*mut u8, usize, usize, u8)> = Vec::new();
        for step in 0..20_000 {
            if live.is_empty() || random(3) != 0 {
                let size = match random(10) {
                    0 => random(16 * 1024) as usize + 1,
                    1..=3 => random(2048) as usize + 1,
                    _ => random(64) as usize + 1,
                };
                let align = 1 << random(7);
                let ptr = arena.alloc(size, align);
                if ptr.is_null() {
                    continue;
                }
                assert_eq!(ptr as usize % align, 0);
                assert!(arena.contains(ptr, size));
                let fill = step as u8;
                unsafe { ptr::write_bytes(ptr, fill, size) };
                for &(other, other_size, _, _) in &live {
                    let disjoint = ptr as usize + size <= other as usize || other as usize + other_size <= ptr as usize;
                    assert!(disjoint, "overlapping blocks at step {step}");
                }
                live.push((ptr, size, align, fill));
            } else {
                let (ptr, size, align, fill) = live.swap_remove(random(live.len() as u64));
                let data = unsafe { core::slice::from_raw_parts(ptr, size) };
                assert!(data.iter().all(|&b| b == fill), "data corrupted at step {step}");
                arena.free(ptr, size, align);
            }
        }
        for (ptr, size, align, _) in live {
            arena.free(ptr, size, align);
        }
        // after freeing everything the whole heap is usable for one allocation again
        assert!(!arena.alloc(200 * 1024, 8).is_null());
    }
}
