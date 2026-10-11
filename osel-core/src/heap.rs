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
        if !ptr.is_null() || self.reclaim_free_blocks() == 0 {
            return ptr;
        }
        self.try_allocate(layout)
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
            while let Some(node) = NonNull::new(self.list_heads[index]) {
                self.list_heads[index] = unsafe { node.as_ref().next };
                unsafe { self.fallback_allocator.deallocate(node.cast(), block_layout(index)) };
                released += 1;
            }
            self.free_blocks[index] = 0;
        }
        released
    }
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
