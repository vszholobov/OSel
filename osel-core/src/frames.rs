//! Physical frames that can be handed out and taken back
//!
//! `FramePool` takes fresh frames from `RegionFrameAllocator` and keeps freed ones in a
//! LIFO list stored inside the free frames themselves, so freeing never touches the heap.
//! A bitmap marks the frames on the list: it catches double frees and frames that never
//! came from the pool.

use crate::memory::{LOW_MEMORY_END, PAGE_SIZE, PHYS_ADDR_LIMIT, RegionFrameAllocator};
use bootloader_api::info::{MemoryRegion, MemoryRegionKind};
use x86_64::PhysAddr;
use x86_64::structures::paging::{FrameAllocator, FrameDeallocator, PhysFrame, Size4KiB};

/// Hands out physical frames and takes them back
pub trait FrameSource: FrameAllocator<Size4KiB> + FrameDeallocator<Size4KiB> {
    /// Frames handed out and not given back yet
    fn used_frames(&self) -> usize;
}

/// The pool never hands out memory this far above its lowest frame, so a broken or
/// huge memory map can't make the bitmap bigger than 2 MiB
pub const MAX_TRACKED_BYTES: u64 = 64 * 1024 * 1024 * 1024;

/// End of the free list
const LIST_END: u64 = 0;

pub struct FramePool<'a> {
    fresh: RegionFrameAllocator<'a>,
    /// Virtual address where physical address 0 is mapped
    phys_offset: u64,
    /// Most recently freed frame, `LIST_END` if none
    free_head: u64,
    free_len: usize,
    used: usize,
    /// First byte the bitmap covers
    base: u64,
    /// End of the memory the pool hands out, exclusive
    end: u64,
    /// One bit per frame from `base`: set while the frame is on the free list
    bitmap: Option<&'a mut [u64]>,
}

impl<'a> FramePool<'a> {
    /// A pool that works as a bump allocator until `enable_tracking`; freeing panics till then.
    ///
    /// # Safety
    /// Frames of regions marked usable must really be unused, and all physical memory
    /// must be mapped at `phys_offset`.
    pub unsafe fn new(regions: &'a [MemoryRegion], phys_offset: u64) -> Self {
        let usable = regions.iter().filter(|r| r.kind == MemoryRegionKind::Usable && r.start < r.end);
        let base = usable.clone().map(|r| r.start.max(LOW_MEMORY_END)).min().unwrap_or(LOW_MEMORY_END);
        let base = base.min(PHYS_ADDR_LIMIT) & !(PAGE_SIZE - 1);
        let top = usable.map(|r| r.end.min(PHYS_ADDR_LIMIT)).max().unwrap_or(base).max(base);
        let end = top.min(base.saturating_add(MAX_TRACKED_BYTES));
        // SAFETY: forwarded from the caller
        let fresh = unsafe { RegionFrameAllocator::with_limit(regions, end) };
        FramePool { fresh, phys_offset, free_head: LIST_END, free_len: 0, used: 0, base, end, bitmap: None }
    }

    /// Number of frames a pool over `regions` hands out in total
    pub fn count_frames(regions: &[MemoryRegion]) -> usize {
        // SAFETY: the frames are only counted, and nothing is freed, so nothing is written
        let mut pool = unsafe { FramePool::new(regions, 0) };
        core::iter::from_fn(|| pool.allocate_frame()).count()
    }

    /// Number of `u64` words `enable_tracking` needs
    pub fn bitmap_words(&self) -> usize {
        ((self.end - self.base) / PAGE_SIZE).div_ceil(64) as usize
    }

    /// Starts tracking frames so they can be freed. `bitmap` must have `bitmap_words`
    /// words; it is zeroed here.
    pub fn enable_tracking(&mut self, bitmap: &'a mut [u64]) {
        assert!(self.bitmap.is_none(), "frame tracking is already enabled");
        assert!(bitmap.len() >= self.bitmap_words(), "the frame bitmap is too small");
        bitmap.fill(0);
        self.bitmap = Some(bitmap);
    }

    /// Frames waiting on the free list
    pub fn free_list_len(&self) -> usize {
        self.free_len
    }

    /// Whether `address` is the start of a frame that is handed out right now
    pub fn is_handed_out(&self, address: u64) -> bool {
        self.fresh.has_handed_out(address) && !self.is_listed(address)
    }

    fn bit(&self, address: u64) -> (usize, u64) {
        let index = (address - self.base) / PAGE_SIZE;
        ((index / 64) as usize, 1 << (index % 64))
    }

    fn is_listed(&self, address: u64) -> bool {
        let (word, mask) = self.bit(address);
        self.bitmap.as_ref().is_some_and(|bitmap| bitmap[word] & mask != 0)
    }

    fn link(&self, frame: u64) -> *mut u64 {
        (self.phys_offset + frame) as *mut u64
    }
}

unsafe impl FrameAllocator<Size4KiB> for FramePool<'_> {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        let address = if self.free_head != LIST_END {
            let address = self.free_head;
            // a write after free may have overwritten the link
            let in_span = (self.base..self.end).contains(&address) && address % PAGE_SIZE == 0;
            assert!(in_span, "free frame list is corrupted: link {address:#x}");
            let (word, mask) = self.bit(address);
            let bitmap = self.bitmap.as_mut().expect("free frames without a bitmap");
            assert!(bitmap[word] & mask != 0, "free frame list is corrupted: link {address:#x}");
            bitmap[word] &= !mask;
            // SAFETY: the frame is free, its first word holds the next free frame
            self.free_head = unsafe { self.link(address).read() };
            self.free_len -= 1;
            address
        } else {
            self.fresh.allocate_frame()?.start_address().as_u64()
        };
        self.used += 1;
        Some(PhysFrame::containing_address(PhysAddr::new(address)))
    }
}

impl FrameDeallocator<Size4KiB> for FramePool<'_> {
    /// Puts the frame on the free list. Panics on a double free and on a frame the
    /// pool never handed out.
    unsafe fn deallocate_frame(&mut self, frame: PhysFrame) {
        let address = frame.start_address().as_u64();
        assert!(self.bitmap.is_some(), "frame {address:#x} freed before frame tracking is enabled");
        assert!(self.is_handed_out(address), "frame {address:#x} is not handed out: double free or a foreign frame");
        let (word, mask) = self.bit(address);
        self.bitmap.as_mut().expect("checked above")[word] |= mask;
        // SAFETY: the caller doesn't use the frame anymore, it now holds the list link
        unsafe { self.link(address).write(self.free_head) };
        self.free_head = address;
        self.free_len += 1;
        self.used -= 1;
    }
}

impl FrameSource for FramePool<'_> {
    fn used_frames(&self) -> usize {
        self.used
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeSet;
    use alloc::vec;
    use alloc::vec::Vec;
    use std::alloc::{Layout, alloc_zeroed, dealloc};

    const MIB: u64 = 1024 * 1024;

    fn usable(start: u64, end: u64) -> MemoryRegion {
        MemoryRegion { start, end, kind: MemoryRegionKind::Usable }
    }

    fn reserved(start: u64, end: u64) -> MemoryRegion {
        MemoryRegion { start, end, kind: MemoryRegionKind::Bootloader }
    }

    /// Host memory standing in for physical addresses `MIB..MIB + frames * PAGE_SIZE`
    struct Ram {
        memory: *mut u8,
        layout: Layout,
    }

    impl Ram {
        fn new(frames: u64) -> Ram {
            let layout = Layout::from_size_align((frames * PAGE_SIZE) as usize, PAGE_SIZE as usize).unwrap();
            let memory = unsafe { alloc_zeroed(layout) };
            assert!(!memory.is_null());
            Ram { memory, layout }
        }

        /// Physical memory "mapped" so that `MIB` lands on the buffer
        fn phys_offset(&self) -> u64 {
            self.memory as u64 - MIB
        }
    }

    impl Drop for Ram {
        fn drop(&mut self) {
            unsafe { dealloc(self.memory, self.layout) };
        }
    }

    fn address(frame: PhysFrame) -> u64 {
        frame.start_address().as_u64()
    }

    fn frame(address: u64) -> PhysFrame {
        PhysFrame::containing_address(PhysAddr::new(address))
    }

    /// Runs `test` with a tracking pool over `regions`, all inside `frames` frames from 1 MiB
    fn with_pool(frames: u64, regions: &[MemoryRegion], test: impl FnOnce(&mut FramePool)) {
        let ram = Ram::new(frames);
        let mut pool = unsafe { FramePool::new(regions, ram.phys_offset()) };
        // garbage on purpose: `enable_tracking` must clear it
        let mut bitmap = vec![u64::MAX; pool.bitmap_words()];
        pool.enable_tracking(&mut bitmap);
        test(&mut pool);
    }

    #[test]
    fn freed_frames_come_back_last_in_first_out() {
        with_pool(8, &[usable(MIB, MIB + 8 * PAGE_SIZE)], |pool| {
            let (a, b) = (pool.allocate_frame().unwrap(), pool.allocate_frame().unwrap());
            assert_ne!(a, b);
            assert_eq!(pool.used_frames(), 2);
            unsafe { pool.deallocate_frame(a) };
            unsafe { pool.deallocate_frame(b) };
            assert_eq!(pool.used_frames(), 0);
            assert_eq!(pool.free_list_len(), 2);
            assert_eq!(pool.allocate_frame(), Some(b));
            assert_eq!(pool.allocate_frame(), Some(a));
            assert_eq!(address(pool.allocate_frame().unwrap()), MIB + 2 * PAGE_SIZE, "then fresh frames");
        });
    }

    /// Nikka 2-mm-1 `basic`: a freed frame is the next one handed out
    #[test]
    fn a_freed_frame_is_reused_right_away() {
        with_pool(8, &[usable(MIB, MIB + 8 * PAGE_SIZE)], |pool| {
            let a = pool.allocate_frame().unwrap();
            let _b = pool.allocate_frame().unwrap();
            unsafe { pool.deallocate_frame(a) };
            assert_eq!(pool.allocate_frame(), Some(a));
        });
    }

    /// Nikka 2-mm-1 `allocated_frames_are_not_used`: every frame once, all of them
    /// back after freeing, and again the same set
    #[test]
    fn all_frames_round_trip() {
        let regions = [usable(MIB, MIB + 5 * PAGE_SIZE), reserved(MIB + 5 * PAGE_SIZE, MIB + 6 * PAGE_SIZE), usable(MIB + 6 * PAGE_SIZE, MIB + 16 * PAGE_SIZE)];
        with_pool(16, &regions, |pool| {
            let first: Vec<PhysFrame> = core::iter::from_fn(|| pool.allocate_frame()).collect();
            assert_eq!(first.len(), 15);
            assert_eq!(FramePool::count_frames(&regions), 15);
            assert_eq!(pool.used_frames(), 15);
            assert!(pool.allocate_frame().is_none(), "exhaustion is sticky");
            for &frame in &first {
                unsafe { pool.deallocate_frame(frame) };
            }
            assert_eq!(pool.used_frames(), 0);
            let second: Vec<PhysFrame> = core::iter::from_fn(|| pool.allocate_frame()).collect();
            let reversed: Vec<PhysFrame> = first.iter().rev().copied().collect();
            assert_eq!(second, reversed, "LIFO order");
            assert!(pool.allocate_frame().is_none());
        });
    }

    #[test]
    fn the_free_list_lives_in_the_frames() {
        let ram = Ram::new(4);
        let regions = [usable(MIB, MIB + 4 * PAGE_SIZE)];
        let mut pool = unsafe { FramePool::new(&regions, ram.phys_offset()) };
        let mut bitmap = vec![0; pool.bitmap_words()];
        pool.enable_tracking(&mut bitmap);
        let frames: Vec<PhysFrame> = (0..3).map(|_| pool.allocate_frame().unwrap()).collect();
        for &frame in &frames {
            unsafe { pool.deallocate_frame(frame) };
        }
        // the newest free frame links to the previous one, the oldest ends the list
        let link = |frame: PhysFrame| unsafe { ((ram.phys_offset() + address(frame)) as *const u64).read() };
        assert_eq!(link(frames[2]), address(frames[1]));
        assert_eq!(link(frames[1]), address(frames[0]));
        assert_eq!(link(frames[0]), LIST_END);
    }

    #[test]
    #[should_panic(expected = "double free or a foreign frame")]
    fn double_free_panics() {
        with_pool(4, &[usable(MIB, MIB + 4 * PAGE_SIZE)], |pool| {
            let a = pool.allocate_frame().unwrap();
            unsafe { pool.deallocate_frame(a) };
            unsafe { pool.deallocate_frame(a) };
        });
    }

    #[test]
    #[should_panic(expected = "double free or a foreign frame")]
    fn double_free_deep_in_the_list_panics() {
        with_pool(4, &[usable(MIB, MIB + 4 * PAGE_SIZE)], |pool| {
            let a = pool.allocate_frame().unwrap();
            let b = pool.allocate_frame().unwrap();
            unsafe { pool.deallocate_frame(a) };
            unsafe { pool.deallocate_frame(b) };
            unsafe { pool.deallocate_frame(a) };
        });
    }

    #[test]
    #[should_panic(expected = "double free or a foreign frame")]
    fn freeing_a_frame_never_handed_out_panics() {
        with_pool(4, &[usable(MIB, MIB + 4 * PAGE_SIZE)], |pool| {
            let _a = pool.allocate_frame().unwrap();
            unsafe { pool.deallocate_frame(frame(MIB + 2 * PAGE_SIZE)) };
        });
    }

    #[test]
    fn foreign_frames_are_refused() {
        let regions = [usable(MIB, MIB + 4 * PAGE_SIZE), reserved(MIB + 4 * PAGE_SIZE, MIB + 5 * PAGE_SIZE), usable(MIB + 5 * PAGE_SIZE, MIB + 6 * PAGE_SIZE)];
        for foreign in [0, 0xa_0000, MIB - PAGE_SIZE, MIB + 4 * PAGE_SIZE, MIB + 6 * PAGE_SIZE, 0xfee0_0000, PHYS_ADDR_LIMIT - PAGE_SIZE] {
            let result = std::panic::catch_unwind(|| {
                with_pool(6, &regions, |pool| {
                    while pool.allocate_frame().is_some() {}
                    unsafe { pool.deallocate_frame(frame(foreign)) };
                })
            });
            let message = *result.expect_err("a foreign frame was accepted").downcast::<String>().unwrap();
            assert!(message.contains("double free or a foreign frame"), "{foreign:#x}: {message}");
        }
    }

    /// A write to a freed frame breaks the list, the next allocation notices
    #[test]
    fn a_corrupted_link_is_caught() {
        let ram = Ram::new(4);
        let regions = [usable(MIB, MIB + 4 * PAGE_SIZE)];
        for link in [MIB / 2, MIB + PAGE_SIZE + 8, MIB + 2 * PAGE_SIZE, MIB + 100 * PAGE_SIZE] {
            let result = std::panic::catch_unwind(|| {
                let mut pool = unsafe { FramePool::new(&regions, ram.phys_offset()) };
                let mut bitmap = vec![0; pool.bitmap_words()];
                pool.enable_tracking(&mut bitmap);
                let (a, _b, _c) = (pool.allocate_frame().unwrap(), pool.allocate_frame().unwrap(), pool.allocate_frame().unwrap());
                unsafe { pool.deallocate_frame(a) };
                unsafe { ((ram.phys_offset() + address(a)) as *mut u64).write(link) };
                pool.allocate_frame();
                pool.allocate_frame();
            });
            let message = *result.expect_err("corruption went unnoticed").downcast::<String>().unwrap();
            assert!(message.contains("free frame list is corrupted"), "{link:#x}: {message}");
        }
    }

    #[test]
    #[should_panic(expected = "freed before frame tracking is enabled")]
    fn freeing_before_tracking_panics() {
        let ram = Ram::new(4);
        let regions = [usable(MIB, MIB + 4 * PAGE_SIZE)];
        let mut pool = unsafe { FramePool::new(&regions, ram.phys_offset()) };
        let a = pool.allocate_frame().unwrap();
        unsafe { pool.deallocate_frame(a) };
    }

    #[test]
    fn frames_handed_out_before_tracking_can_be_freed() {
        let ram = Ram::new(8);
        let regions = [usable(MIB, MIB + 8 * PAGE_SIZE)];
        let mut pool = unsafe { FramePool::new(&regions, ram.phys_offset()) };
        let early: Vec<PhysFrame> = (0..3).map(|_| pool.allocate_frame().unwrap()).collect();
        let mut bitmap = vec![u64::MAX; pool.bitmap_words()];
        pool.enable_tracking(&mut bitmap);
        assert_eq!(pool.used_frames(), 3);
        for &frame in &early {
            assert!(pool.is_handed_out(address(frame)));
            unsafe { pool.deallocate_frame(frame) };
        }
        assert!(!pool.is_handed_out(MIB + 3 * PAGE_SIZE), "not handed out yet");
        assert_eq!(pool.used_frames(), 0);
    }

    #[test]
    #[should_panic(expected = "already enabled")]
    fn tracking_is_enabled_once() {
        with_pool(1, &[usable(MIB, MIB + PAGE_SIZE)], |pool| pool.enable_tracking(&mut []));
    }

    #[test]
    fn bitmap_covers_only_the_usable_span() {
        let regions = [usable(0, 640 * 1024), usable(MIB, 2 * MIB), usable(4 * MIB, 4 * MIB + 64 * PAGE_SIZE), reserved(8 * MIB, u64::MAX)];
        let pool = unsafe { FramePool::new(&regions, 0) };
        assert_eq!(pool.bitmap_words(), ((3 * MIB + 64 * PAGE_SIZE) / PAGE_SIZE).div_ceil(64) as usize);
        assert_eq!(unsafe { FramePool::new(&[], 0) }.bitmap_words(), 0);
    }

    #[test]
    fn bitmap_stays_small_for_a_huge_map() {
        let top = PHYS_ADDR_LIMIT;
        let regions = [usable(MIB, MIB + PAGE_SIZE), usable(top - PAGE_SIZE, top)];
        let mut pool = unsafe { FramePool::new(&regions, 0) };
        assert_eq!(pool.bitmap_words() as u64, MAX_TRACKED_BYTES / PAGE_SIZE / 64);
        assert_eq!(address(pool.allocate_frame().unwrap()), MIB);
        assert!(pool.allocate_frame().is_none(), "memory past the tracked span is never handed out");
        assert_eq!(FramePool::count_frames(&regions), 1);
        let mut bitmap = vec![0; pool.bitmap_words()];
        pool.enable_tracking(&mut bitmap);
        let past_the_span = std::panic::catch_unwind(core::panic::AssertUnwindSafe(|| unsafe {
            pool.deallocate_frame(frame(top - PAGE_SIZE))
        }));
        let message = *past_the_span.expect_err("accepted").downcast::<String>().unwrap();
        assert!(message.contains("double free or a foreign frame"), "{message}");
    }

    /// Random allocations and frees against a model: unique frames, counters match,
    /// frees are LIFO
    #[test]
    fn random_operations_match_a_model() {
        let regions = [usable(MIB, MIB + 20 * PAGE_SIZE), reserved(MIB + 7 * PAGE_SIZE, MIB + 9 * PAGE_SIZE), usable(MIB + 30 * PAGE_SIZE, MIB + 64 * PAGE_SIZE)];
        with_pool(64, &regions, |pool| {
            let mut seed = 0x9e37_79b9_7f4a_7c15u64;
            let mut random = move |limit: u64| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed % limit
            };
            let mut used: Vec<PhysFrame> = Vec::new();
            let mut freed: Vec<PhysFrame> = Vec::new();
            for _ in 0..5000 {
                if random(2) == 0 || used.is_empty() {
                    match pool.allocate_frame() {
                        Some(frame) => {
                            assert!(!used.contains(&frame), "{frame:?} handed out twice");
                            if let Some(expected) = freed.pop() {
                                assert_eq!(frame, expected, "not LIFO");
                            }
                            used.push(frame);
                        }
                        None => assert!(freed.is_empty(), "free frames exist, but none came out"),
                    }
                } else {
                    let frame = used.swap_remove(random(used.len() as u64) as usize);
                    unsafe { pool.deallocate_frame(frame) };
                    freed.push(frame);
                }
                assert_eq!(pool.used_frames(), used.len());
                assert_eq!(pool.free_list_len(), freed.len());
            }
            let unique: BTreeSet<u64> = used.iter().chain(&freed).map(|&f| address(f)).collect();
            assert_eq!(unique.len(), used.len() + freed.len());
            assert!(unique.iter().all(|&a| !(MIB + 7 * PAGE_SIZE..MIB + 9 * PAGE_SIZE).contains(&a)));
        });
    }
}
