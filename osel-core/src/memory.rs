//! Virtual memory layout and physical frame allocation
//!
//! Virtual address space layout:
//! - lower half (`0x0000_...`) is left free for user programs;
//! - `KERNEL_DYNAMIC_START..=KERNEL_DYNAMIC_END`: everything the bootloader places itself
//!   (kernel image, stack, boot info, framebuffer, physical memory mapping);
//! - `HEAP_START..HEAP_START + HEAP_SIZE`: kernel heap.

use bootloader_api::info::{MemoryRegion, MemoryRegionKind};
use x86_64::structures::paging::mapper::MapToError;
use x86_64::structures::paging::{FrameAllocator, Mapper, Page, PageTableFlags, PhysFrame, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};

pub const PAGE_SIZE: u64 = 4096;

/// First address of the higher half
pub const KERNEL_DYNAMIC_START: u64 = 0xffff_8000_0000_0000;
/// Last page the bootloader may use for its own mappings
pub const KERNEL_DYNAMIC_END: u64 = 0xffff_bfff_ffff_f000;
/// Right after the area reserved for the bootloader mappings
pub const HEAP_START: u64 = KERNEL_DYNAMIC_END + PAGE_SIZE;
pub const HEAP_SIZE: u64 = 1024 * 1024;
/// End of the user half of the address space (exclusive)
pub const USER_END: u64 = 0x0000_8000_0000_0000;

/// x86_64 physical addresses have at most 52 bits
pub const PHYS_ADDR_LIMIT: u64 = 1 << 52;

/// Memory below 1 MiB is kept free: application processors start in real mode there
pub const LOW_MEMORY_END: u64 = 0x10_0000;

/// Hands out usable physical frames in ascending order and never reuses them.
///
/// Doesn't rely on the memory map being sorted or free of overlaps: a frame is only
/// handed out if it lies in a usable region and touches no other kind of region.
pub struct RegionFrameAllocator<'a> {
    regions: &'a [MemoryRegion],
    /// Lowest address that may still be handed out
    next: u64,
    allocated: usize,
}

impl<'a> RegionFrameAllocator<'a> {
    /// # Safety
    /// Frames of regions marked usable must really be unused.
    pub unsafe fn new(regions: &'a [MemoryRegion]) -> Self {
        RegionFrameAllocator { regions, next: LOW_MEMORY_END, allocated: 0 }
    }

    /// Number of frames handed out so far
    pub fn allocated(&self) -> usize {
        self.allocated
    }

    /// Lowest frame start `>= from` inside a usable region, ignoring reserved regions
    fn lowest_usable_candidate(&self, from: u64) -> Option<u64> {
        self.regions
            .iter()
            .filter(|r| r.kind == MemoryRegionKind::Usable)
            .filter_map(|r| {
                let start = align_up(r.start.max(from))?;
                // a broken map could describe memory past what the CPU can address
                (start + PAGE_SIZE <= r.end.min(PHYS_ADDR_LIMIT)).then_some(start)
            })
            .min()
    }

    /// End of a non-usable region overlapping the frame at `start`, if any
    fn reserved_overlap_end(&self, start: u64) -> Option<u64> {
        let end = start + PAGE_SIZE;
        self.regions
            .iter()
            .filter(|r| r.kind != MemoryRegionKind::Usable && r.start < end && start < r.end)
            .map(|r| r.end)
            .max()
    }
}

unsafe impl FrameAllocator<Size4KiB> for RegionFrameAllocator<'_> {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        loop {
            let candidate = self.lowest_usable_candidate(self.next)?;
            match self.reserved_overlap_end(candidate) {
                // skip the whole reserved region and look again
                Some(reserved_end) => self.next = reserved_end.max(candidate + PAGE_SIZE),
                None => {
                    self.next = candidate + PAGE_SIZE;
                    self.allocated += 1;
                    return Some(PhysFrame::containing_address(PhysAddr::new(candidate)));
                }
            }
        }
    }
}

fn align_up(address: u64) -> Option<u64> {
    let aligned = address.checked_add(PAGE_SIZE - 1)? & !(PAGE_SIZE - 1);
    (aligned < PHYS_ADDR_LIMIT).then_some(aligned)
}

#[derive(Debug)]
pub enum MapRangeError {
    /// The range overflows the address space or crosses the non-canonical hole
    InvalidRange,
    Map(MapToError<Size4KiB>),
}

impl From<MapToError<Size4KiB>> for MapRangeError {
    fn from(error: MapToError<Size4KiB>) -> Self {
        MapRangeError::Map(error)
    }
}

/// Maps `size` bytes starting at `start` to freshly allocated frames.
/// Pages mapped before an error stay mapped.
pub fn map_range(
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
    start: VirtAddr,
    size: u64,
    flags: PageTableFlags,
) -> Result<(), MapRangeError> {
    if size == 0 {
        return Ok(());
    }
    let last = start.as_u64().checked_add(size - 1).ok_or(MapRangeError::InvalidRange)?;
    let last = VirtAddr::try_new(last).map_err(|_| MapRangeError::InvalidRange)?;
    if (start.as_u64() < USER_END) != (last.as_u64() < USER_END) {
        return Err(MapRangeError::InvalidRange);
    }
    let first = Page::<Size4KiB>::containing_address(start);
    let last = Page::<Size4KiB>::containing_address(last);
    for page in Page::range_inclusive(first, last) {
        let frame = frame_allocator.allocate_frame().ok_or(MapToError::FrameAllocationFailed)?;
        unsafe { mapper.map_to(page, frame, flags, frame_allocator)?.ignore() };
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::boxed::Box;
    use alloc::collections::BTreeSet;
    use alloc::vec::Vec;
    use x86_64::structures::paging::{OffsetPageTable, PageTable, Translate};

    const MIB: u64 = 1024 * 1024;

    fn region(start: u64, end: u64, kind: MemoryRegionKind) -> MemoryRegion {
        MemoryRegion { start, end, kind }
    }

    fn usable(start: u64, end: u64) -> MemoryRegion {
        region(start, end, MemoryRegionKind::Usable)
    }

    fn reserved(start: u64, end: u64) -> MemoryRegion {
        region(start, end, MemoryRegionKind::Bootloader)
    }

    fn all_frames(regions: &[MemoryRegion]) -> Vec<u64> {
        let mut allocator = unsafe { RegionFrameAllocator::new(regions) };
        let frames: Vec<u64> =
            core::iter::from_fn(|| allocator.allocate_frame()).map(|f| f.start_address().as_u64()).collect();
        assert_eq!(allocator.allocated(), frames.len());
        assert!(allocator.allocate_frame().is_none(), "exhaustion is sticky");
        frames
    }

    #[test]
    fn layout_is_consistent() {
        assert!(USER_END <= KERNEL_DYNAMIC_START);
        assert!(KERNEL_DYNAMIC_START < KERNEL_DYNAMIC_END);
        assert!(HEAP_START > KERNEL_DYNAMIC_END, "heap is outside the bootloader area");
        assert_eq!(HEAP_START % PAGE_SIZE, 0);
        assert_eq!(HEAP_SIZE % PAGE_SIZE, 0);
        // canonical addresses: VirtAddr::try_new rejects the others
        for address in [KERNEL_DYNAMIC_START, KERNEL_DYNAMIC_END, HEAP_START, HEAP_START + HEAP_SIZE - 1] {
            assert!(VirtAddr::try_new(address).is_ok(), "{address:#x}");
        }
        assert!(VirtAddr::try_new(USER_END - 1).is_ok());
        // the heap has its own level 4 entry, away from the bootloader entries
        let heap_entry = VirtAddr::new(HEAP_START).p4_index();
        assert_ne!(heap_entry, VirtAddr::new(KERNEL_DYNAMIC_END).p4_index());
        assert_eq!(heap_entry, VirtAddr::new(HEAP_START + HEAP_SIZE - 1).p4_index());
    }

    #[test]
    fn empty_map_has_no_frames() {
        assert!(all_frames(&[]).is_empty());
    }

    #[test]
    fn low_memory_is_never_used() {
        assert!(all_frames(&[usable(0, LOW_MEMORY_END)]).is_empty());
        assert_eq!(all_frames(&[usable(0, LOW_MEMORY_END + 2 * PAGE_SIZE)]), [LOW_MEMORY_END, LOW_MEMORY_END + PAGE_SIZE]);
    }

    #[test]
    fn unaligned_regions_give_whole_frames_only() {
        let frames = all_frames(&[usable(MIB + 1, MIB + 3 * PAGE_SIZE - 1)]);
        assert_eq!(frames, [MIB + PAGE_SIZE]);
        assert!(all_frames(&[usable(MIB + 1, MIB + PAGE_SIZE + 10)]).is_empty(), "tiny region");
        assert!(all_frames(&[usable(MIB, MIB)]).is_empty(), "empty region");
    }

    #[test]
    fn skips_reserved_regions() {
        let frames = all_frames(&[
            usable(MIB, MIB + 2 * PAGE_SIZE),
            reserved(MIB + 2 * PAGE_SIZE, MIB + 4 * PAGE_SIZE),
            usable(MIB + 4 * PAGE_SIZE, MIB + 5 * PAGE_SIZE),
        ]);
        assert_eq!(frames, [MIB, MIB + PAGE_SIZE, MIB + 4 * PAGE_SIZE]);
    }

    #[test]
    fn unsorted_regions_are_all_used() {
        let frames = all_frames(&[usable(2 * MIB, 2 * MIB + PAGE_SIZE), usable(MIB, MIB + PAGE_SIZE)]);
        assert_eq!(frames, [MIB, 2 * MIB]);
    }

    #[test]
    fn overlapping_reserved_region_wins() {
        // firmware maps can contain a reserved range inside a usable one
        let frames = all_frames(&[
            usable(MIB, MIB + 4 * PAGE_SIZE),
            region(MIB + PAGE_SIZE + 16, MIB + PAGE_SIZE + 32, MemoryRegionKind::UnknownBios(2)),
        ]);
        assert_eq!(frames, [MIB, MIB + 2 * PAGE_SIZE, MIB + 3 * PAGE_SIZE]);
    }

    #[test]
    fn overlapping_usable_regions_give_unique_frames() {
        let frames = all_frames(&[usable(MIB, MIB + 3 * PAGE_SIZE), usable(MIB + PAGE_SIZE, MIB + 5 * PAGE_SIZE)]);
        assert_eq!(frames, [MIB, MIB + PAGE_SIZE, MIB + 2 * PAGE_SIZE, MIB + 3 * PAGE_SIZE, MIB + 4 * PAGE_SIZE]);
    }

    #[test]
    fn region_at_the_top_of_physical_memory() {
        let top = PHYS_ADDR_LIMIT;
        assert_eq!(all_frames(&[usable(top - PAGE_SIZE, top)]), [top - PAGE_SIZE]);
        assert_eq!(all_frames(&[usable(top - PAGE_SIZE, top + 10 * PAGE_SIZE)]), [top - PAGE_SIZE], "clamped");
        assert!(all_frames(&[usable(top, u64::MAX)]).is_empty(), "not addressable");
        assert!(all_frames(&[usable(u64::MAX - 10, u64::MAX)]).is_empty(), "no overflow");
    }

    /// Pseudo random memory maps: every frame is unique, aligned, usable and above 1 MiB
    #[test]
    fn random_maps_invariants() {
        let mut seed = 0x1234_5678_9abc_def0u64;
        let mut random = move |limit: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % limit
        };
        for _ in 0..200 {
            let regions: Vec<MemoryRegion> = (0..random(8) + 1)
                .map(|_| {
                    let start = random(64) * PAGE_SIZE / 2 + random(3) * 100;
                    let end = start + random(10) * PAGE_SIZE / 2 + random(2);
                    let kind = if random(3) == 0 { MemoryRegionKind::Bootloader } else { MemoryRegionKind::Usable };
                    region(start, end, kind)
                })
                .collect();
            let regions: Vec<MemoryRegion> =
                regions.into_iter().map(|r| region(r.start + MIB - PAGE_SIZE * 4, r.end + MIB - PAGE_SIZE * 4, r.kind)).collect();
            let frames = all_frames(&regions);
            let unique: BTreeSet<u64> = frames.iter().copied().collect();
            assert_eq!(unique.len(), frames.len(), "duplicate frame in {regions:?}");
            for &frame in &frames {
                assert_eq!(frame % PAGE_SIZE, 0);
                assert!(frame >= LOW_MEMORY_END);
                let end = frame + PAGE_SIZE;
                assert!(regions.iter().any(|r| r.kind == MemoryRegionKind::Usable && r.start <= frame && end <= r.end));
                assert!(!regions.iter().any(|r| r.kind != MemoryRegionKind::Usable && r.start < end && frame < r.end));
            }
        }
    }

    /// Frames backed by real host memory, so page tables built in them can be walked
    struct HostFrames {
        handed_out: usize,
        limit: usize,
    }

    unsafe impl FrameAllocator<Size4KiB> for HostFrames {
        fn allocate_frame(&mut self) -> Option<PhysFrame> {
            if self.handed_out == self.limit {
                return None;
            }
            self.handed_out += 1;
            let table = Box::leak(Box::new(PageTable::new()));
            Some(PhysFrame::containing_address(PhysAddr::new(table as *mut PageTable as u64)))
        }
    }

    fn host_mapper() -> OffsetPageTable<'static> {
        let level_4 = Box::leak(Box::new(PageTable::new()));
        // with offset 0 a "physical" address is the host address itself
        unsafe { OffsetPageTable::new(level_4, VirtAddr::new(0)) }
    }

    #[test]
    fn map_range_maps_every_page() {
        let mut mapper = host_mapper();
        let mut frames = HostFrames { handed_out: 0, limit: 100 };
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
        let start = VirtAddr::new(HEAP_START);
        map_range(&mut mapper, &mut frames, start, 3 * PAGE_SIZE + 1, flags).unwrap();

        for page in 0..4 {
            assert!(mapper.translate_addr(start + page * PAGE_SIZE).is_some(), "page {page}");
        }
        assert!(mapper.translate_addr(start + 4 * PAGE_SIZE).is_none());
        assert!(mapper.translate_addr(start - 1u64).is_none());
        let page = Page::<Size4KiB>::containing_address(start);
        let (_, page_flags) = match mapper.translate(page.start_address()) {
            x86_64::structures::paging::mapper::TranslateResult::Mapped { flags, .. } => ((), flags),
            other => panic!("{other:?}"),
        };
        assert!(page_flags.contains(flags));
        // 4 data frames plus level 3, 2 and 1 tables
        assert_eq!(frames.handed_out, 7);
    }

    #[test]
    fn map_range_of_nothing() {
        let mut mapper = host_mapper();
        let mut frames = HostFrames { handed_out: 0, limit: 0 };
        map_range(&mut mapper, &mut frames, VirtAddr::new(HEAP_START), 0, PageTableFlags::PRESENT).unwrap();
    }

    #[test]
    fn map_range_reports_overlap() {
        let mut mapper = host_mapper();
        let mut frames = HostFrames { handed_out: 0, limit: 100 };
        let start = VirtAddr::new(0x40_0000);
        map_range(&mut mapper, &mut frames, start, PAGE_SIZE, PageTableFlags::PRESENT).unwrap();
        let error = map_range(&mut mapper, &mut frames, start, PAGE_SIZE, PageTableFlags::PRESENT).unwrap_err();
        assert!(matches!(error, MapRangeError::Map(MapToError::PageAlreadyMapped(_))), "{error:?}");
    }

    #[test]
    fn map_range_reports_out_of_frames() {
        let mut mapper = host_mapper();
        let mut frames = HostFrames { handed_out: 0, limit: 3 };
        let error =
            map_range(&mut mapper, &mut frames, VirtAddr::new(0x40_0000), 4 * PAGE_SIZE, PageTableFlags::PRESENT)
                .unwrap_err();
        assert!(matches!(error, MapRangeError::Map(MapToError::FrameAllocationFailed)), "{error:?}");
    }

    #[test]
    fn map_range_of_whole_pages_maps_no_extra_page() {
        let mut mapper = host_mapper();
        let mut frames = HostFrames { handed_out: 0, limit: 100 };
        let start = VirtAddr::new(HEAP_START);
        map_range(&mut mapper, &mut frames, start, 2 * PAGE_SIZE, PageTableFlags::PRESENT).unwrap();
        assert!(mapper.translate_addr(start + PAGE_SIZE).is_some());
        assert!(mapper.translate_addr(start + 2 * PAGE_SIZE).is_none());
        // 2 data frames plus level 3, 2 and 1 tables
        assert_eq!(frames.handed_out, 5);
    }

    #[test]
    fn map_range_with_unaligned_start() {
        let mut mapper = host_mapper();
        let mut frames = HostFrames { handed_out: 0, limit: 100 };
        let start = VirtAddr::new(HEAP_START + PAGE_SIZE - 1);
        map_range(&mut mapper, &mut frames, start, 2, PageTableFlags::PRESENT).unwrap();
        assert!(mapper.translate_addr(VirtAddr::new(HEAP_START)).is_some());
        assert!(mapper.translate_addr(VirtAddr::new(HEAP_START + PAGE_SIZE)).is_some());
        assert!(mapper.translate_addr(VirtAddr::new(HEAP_START + 2 * PAGE_SIZE)).is_none());
    }

    #[test]
    fn map_range_refuses_invalid_ranges() {
        let mut mapper = host_mapper();
        let mut frames = HostFrames { handed_out: 0, limit: 100 };
        let flags = PageTableFlags::PRESENT;
        let crossing_hole = map_range(&mut mapper, &mut frames, VirtAddr::new(USER_END - PAGE_SIZE), 2 * PAGE_SIZE, flags);
        assert!(matches!(crossing_hole, Err(MapRangeError::InvalidRange)));
        let past_the_end = map_range(&mut mapper, &mut frames, VirtAddr::new(u64::MAX - PAGE_SIZE + 1), 2 * PAGE_SIZE, flags);
        assert!(matches!(past_the_end, Err(MapRangeError::InvalidRange)));
        assert_eq!(frames.handed_out, 0, "nothing is mapped for a bad range");
    }

    #[test]
    fn reserved_region_up_to_the_end() {
        let frames = all_frames(&[usable(MIB, MIB + 4 * PAGE_SIZE), reserved(MIB + PAGE_SIZE, u64::MAX)]);
        assert_eq!(frames, [MIB]);
    }

    #[test]
    fn inverted_region_is_harmless() {
        let frames = all_frames(&[usable(MIB, MIB + 3 * PAGE_SIZE), reserved(MIB + 2 * PAGE_SIZE, MIB + PAGE_SIZE)]);
        assert_eq!(frames, [MIB, MIB + PAGE_SIZE, MIB + 2 * PAGE_SIZE]);
        assert!(all_frames(&[usable(MIB + PAGE_SIZE, MIB)]).is_empty());
    }

    #[test]
    fn touching_usable_regions() {
        let frames = all_frames(&[usable(MIB + 2 * PAGE_SIZE, MIB + 3 * PAGE_SIZE), usable(MIB, MIB + 2 * PAGE_SIZE)]);
        assert_eq!(frames, [MIB, MIB + PAGE_SIZE, MIB + 2 * PAGE_SIZE]);
    }
}
