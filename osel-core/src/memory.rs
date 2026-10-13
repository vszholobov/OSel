//! Virtual memory layout and physical frame allocation
//!
//! Virtual address space layout:
//! - lower half (`0x0000_...`) is left free for user programs;
//! - `KERNEL_DYNAMIC_START..=KERNEL_DYNAMIC_END`: everything the bootloader places itself
//!   (kernel image, stack, boot info, framebuffer, physical memory mapping);
//! - `HEAP_START..HEAP_START + HEAP_SIZE`: kernel heap.

use bootloader_api::info::{MemoryRegion, MemoryRegionKind};
use crate::frames::FrameSource;
use x86_64::structures::paging::mapper::MapToError;
use x86_64::structures::paging::{FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};

pub const PAGE_SIZE: u64 = 4096;

/// First address of the higher half
pub const KERNEL_DYNAMIC_START: u64 = 0xffff_8000_0000_0000;
/// Last page the bootloader may use for its own mappings
pub const KERNEL_DYNAMIC_END: u64 = 0xffff_bfff_ffff_f000;
/// Right after the area reserved for the bootloader mappings
pub const HEAP_START: u64 = KERNEL_DYNAMIC_END + PAGE_SIZE;
pub const HEAP_SIZE: u64 = 16 * 1024 * 1024;
/// End of the user half of the address space (exclusive)
pub const USER_END: u64 = 0x0000_8000_0000_0000;

/// x86_64 physical addresses have at most 52 bits
pub const PHYS_ADDR_LIMIT: u64 = 1 << 52;

/// Memory below 1 MiB is kept free: application processors start in real mode there
pub const LOW_MEMORY_END: u64 = 0x10_0000;

/// Hands out usable physical frames in ascending order and never reuses them.
/// Freed frames are reused by `frames::FramePool`, which takes fresh frames from here.
///
/// Doesn't rely on the memory map being sorted or free of overlaps: a frame is only
/// handed out if it lies in a usable region and touches no other kind of region.
pub struct RegionFrameAllocator<'a> {
    regions: &'a [MemoryRegion],
    /// Lowest address that may still be handed out
    next: u64,
    /// Frames end at or below this address
    limit: u64,
    allocated: usize,
}

impl<'a> RegionFrameAllocator<'a> {
    /// # Safety
    /// Frames of regions marked usable must really be unused.
    pub unsafe fn new(regions: &'a [MemoryRegion]) -> Self {
        unsafe { Self::with_limit(regions, PHYS_ADDR_LIMIT) }
    }

    /// Like `new`, but never hands out memory at or above `limit`
    ///
    /// # Safety
    /// Frames of regions marked usable must really be unused.
    pub unsafe fn with_limit(regions: &'a [MemoryRegion], limit: u64) -> Self {
        RegionFrameAllocator { regions, next: LOW_MEMORY_END, limit: limit.min(PHYS_ADDR_LIMIT), allocated: 0 }
    }

    /// Number of frames an allocator over `regions` hands out in total
    pub fn count_frames(regions: &[MemoryRegion]) -> usize {
        // SAFETY: the frames are only counted, never handed to anybody
        let mut allocator = unsafe { RegionFrameAllocator::new(regions) };
        core::iter::from_fn(|| allocator.allocate_frame()).count()
    }

    /// Number of frames handed out so far
    pub fn allocated(&self) -> usize {
        self.allocated
    }

    /// Whether the frame at `address` was already handed out by this allocator
    pub fn has_handed_out(&self, address: u64) -> bool {
        address < self.next && self.is_usable_frame(address)
    }

    /// Whether this allocator hands out the frame at `address` at some point
    pub fn is_usable_frame(&self, address: u64) -> bool {
        address >= LOW_MEMORY_END
            && self.lowest_usable_candidate(address) == Some(address)
            && self.reserved_overlap_end(address).is_none()
    }

    /// Lowest frame start `>= from` inside a usable region, ignoring reserved regions
    fn lowest_usable_candidate(&self, from: u64) -> Option<u64> {
        self.regions
            .iter()
            .filter(|r| r.kind == MemoryRegionKind::Usable)
            .filter_map(|r| {
                let start = align_up(r.start.max(from))?;
                // a broken map could describe memory past what the CPU can address
                (start.checked_add(PAGE_SIZE)? <= r.end.min(self.limit)).then_some(start)
            })
            .min()
    }

    /// End of a non-usable region overlapping the frame at `start`, if any
    fn reserved_overlap_end(&self, start: u64) -> Option<u64> {
        let end = start.saturating_add(PAGE_SIZE);
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
    /// User pages must be in the lower half and user accessible, kernel pages must not
    /// be: a user accessible kernel page would also open the kernel's shared page tables
    PermissionDenied,
    Map(MapToError<Size4KiB>),
}

/// What a system call reports: running out of frames is `ENOMEM`, the rest is a bad request
impl From<MapRangeError> for crate::errno::Errno {
    fn from(error: MapRangeError) -> Self {
        match error {
            MapRangeError::Map(MapToError::FrameAllocationFailed) => crate::errno::Errno::ENOMEM,
            _ => crate::errno::Errno::EINVAL,
        }
    }
}

impl From<MapToError<Size4KiB>> for MapRangeError {
    fn from(error: MapToError<Size4KiB>) -> Self {
        MapRangeError::Map(error)
    }
}

/// Maps `size` bytes starting at `start` to freshly allocated, zeroed frames.
///
/// On error nothing of this call stays: the pages it mapped are unmapped and their frames
/// freed, and empty page tables in the range are freed (see `prune_empty_tables`). The CPU
/// may cache present translations speculatively, so if the table is active, the caller
/// flushes the TLB after an error, as after `unmap_range`.
pub fn map_range(
    mapper: &mut OffsetPageTable,
    frames: &mut impl FrameSource,
    start: VirtAddr,
    size: u64,
    flags: PageTableFlags,
) -> Result<(), MapRangeError> {
    let Some((first, last)) = check_range(start, size)? else {
        return Ok(());
    };
    let user_range = first.start_address().as_u64() < USER_END;
    if user_range != flags.contains(PageTableFlags::USER_ACCESSIBLE) {
        return Err(MapRangeError::PermissionDenied);
    }
    // a global user page would survive the switch to another address space
    if user_range && flags.contains(PageTableFlags::GLOBAL) {
        return Err(MapRangeError::PermissionDenied);
    }
    for page in pages(first, last) {
        if let Err(error) = map_zeroed_page(mapper, frames, page, flags) {
            if page > first {
                unmap_pages(mapper, frames, first, page - 1u64);
            }
            prune_empty_tables(mapper, frames, first, last);
            return Err(error);
        }
    }
    Ok(())
}

/// Unmaps the mapped pages of `size` bytes at `start` and frees their frames, then frees
/// the page tables left empty. Pages that are not mapped are skipped. Returns the number
/// of unmapped pages; flushing their TLB entries is up to the caller.
pub fn unmap_range(
    mapper: &mut OffsetPageTable,
    frames: &mut impl FrameSource,
    start: VirtAddr,
    size: u64,
) -> Result<usize, MapRangeError> {
    let Some((first, last)) = check_range(start, size)? else {
        return Ok(0);
    };
    let unmapped = unmap_pages(mapper, frames, first, last);
    prune_empty_tables(mapper, frames, first, last);
    Ok(unmapped)
}

/// First and last page of a non-empty range that stays in one half of the address space
fn check_range(start: VirtAddr, size: u64) -> Result<Option<(Page, Page)>, MapRangeError> {
    if size == 0 {
        return Ok(None);
    }
    let last = start.as_u64().checked_add(size - 1).ok_or(MapRangeError::InvalidRange)?;
    let last = VirtAddr::try_new(last).map_err(|_| MapRangeError::InvalidRange)?;
    if (start.as_u64() < USER_END) != (last.as_u64() < USER_END) {
        return Err(MapRangeError::InvalidRange);
    }
    Ok(Some((Page::containing_address(start), Page::containing_address(last))))
}

fn map_zeroed_page(
    mapper: &mut OffsetPageTable,
    frames: &mut impl FrameSource,
    page: Page,
    flags: PageTableFlags,
) -> Result<(), MapRangeError> {
    let frame = frames.allocate_frame().ok_or(MapToError::FrameAllocationFailed)?;
    // frames come back from the pool with the data of their previous owner
    let bytes = (mapper.phys_offset() + frame.start_address().as_u64()).as_mut_ptr::<u8>();
    unsafe { bytes.write_bytes(0, PAGE_SIZE as usize) };
    // `map_to` refuses pages that are already mapped, and the CPU never caches
    // not present translations, so there is nothing stale to flush. Tables of the lower
    // half allow everything, the rights of a page are in its own entry only: `mprotect`
    // changes the leaves, and a table made for a read-only page must not stay read-only.
    let result = if page.start_address().as_u64() < USER_END {
        unsafe { mapper.map_to_with_table_flags(page, frame, flags, USER_TABLE_FLAGS, frames) }
    } else {
        unsafe { mapper.map_to(page, frame, flags, frames) }
    };
    match result {
        Ok(flush) => {
            flush.ignore();
            Ok(())
        }
        Err(error) => {
            if frames.can_free(frame) {
                unsafe { frames.deallocate_frame(frame) };
            }
            Err(error.into())
        }
    }
}

/// Flags of every page table of the lower half
pub const USER_TABLE_FLAGS: PageTableFlags =
    PageTableFlags::PRESENT.union(PageTableFlags::WRITABLE).union(PageTableFlags::USER_ACCESSIBLE);

/// Gives every page of `size` bytes at `start` that has a frame the leaf flags `flags`,
/// keeping its frame, also when `flags` has no `PRESENT` (`PROT_NONE`). Pages without a
/// frame are skipped. Returns the number of changed pages; flushing is up to the caller.
pub fn protect_range(mapper: &mut OffsetPageTable, start: VirtAddr, size: u64, flags: PageTableFlags) -> Result<usize, MapRangeError> {
    let Some((first, last)) = check_range(start, size)? else {
        return Ok(0);
    };
    let span = Span { first: linear(first.start_address()), last: linear(last.start_address()) };
    let phys_offset = mapper.phys_offset();
    Ok(protect_level(mapper.level_4_table_mut(), 4, 0, span, phys_offset, flags))
}

fn protect_level(table: &mut PageTable, level: u8, base: u64, span: Span, phys_offset: VirtAddr, flags: PageTableFlags) -> usize {
    let size = entry_size(level);
    let mut changed = 0;
    for (index, entry) in table.iter_mut().enumerate() {
        let start = base + index as u64 * size;
        if start + size - 1 < span.first || start > span.last {
            continue;
        }
        if entry.is_unused() || entry.flags().contains(PageTableFlags::HUGE_PAGE) {
            continue;
        }
        if level == 1 {
            let address = entry.addr();
            entry.set_addr(address, flags);
            changed += 1;
        } else {
            let child = unsafe { table_at(phys_offset, entry.addr()) };
            changed += protect_level(child, level - 1, start, span, phys_offset, flags);
        }
    }
    changed
}

/// Pages `first..=last`. `Page::range_inclusive` steps past the last page of the lower
/// half into the non-canonical hole and panics.
fn pages(first: Page, last: Page) -> impl Iterator<Item = Page> {
    (0..=last - first).map(move |index| first + index)
}

/// Unmaps the 4 KiB pages of `first..=last` by walking the tables, so absent subtrees of a
/// huge range cost nothing. Huge pages are skipped: they are not ours to split.
fn unmap_pages(mapper: &mut OffsetPageTable, frames: &mut impl FrameSource, first: Page, last: Page) -> usize {
    let phys_offset = mapper.phys_offset();
    let span = Span { first: linear(first.start_address()), last: linear(last.start_address()) };
    unmap_level(mapper.level_4_table_mut(), 4, 0, span, phys_offset, frames)
}

fn unmap_level(table: &mut PageTable, level: u8, base: u64, span: Span, phys_offset: VirtAddr, frames: &mut impl FrameSource) -> usize {
    let size = entry_size(level);
    let mut unmapped = 0;
    for (index, entry) in table.iter_mut().enumerate() {
        let start = base + index as u64 * size;
        if start + size - 1 < span.first || start > span.last {
            continue;
        }
        if entry.is_unused() || entry.flags().contains(PageTableFlags::HUGE_PAGE) {
            continue;
        }
        if level == 1 {
            let frame = PhysFrame::containing_address(entry.addr());
            entry.set_unused();
            if frames.can_free(frame) {
                unsafe { frames.deallocate_frame(frame) };
            }
            unmapped += 1;
        } else {
            let child = unsafe { table_at(phys_offset, entry.addr()) };
            unmapped += unmap_level(child, level - 1, start, span, phys_offset, frames);
        }
    }
    unmapped
}

/// Frees the empty page tables that cover pages `first..=last`. Only frames the pool
/// can take back are freed (not the bootloader's), huge page entries are skipped, and
/// level 3 tables of the upper half stay: every address space shares them.
pub fn prune_empty_tables(mapper: &mut OffsetPageTable, frames: &mut impl FrameSource, first: Page, last: Page) {
    let phys_offset = mapper.phys_offset();
    let span = Span { first: linear(first.start_address()), last: linear(last.start_address()) };
    prune_level(mapper.level_4_table_mut(), 4, 0, span, phys_offset, frames);
}

/// Addresses without the sign extension: the upper half starts at `1 << 47`
#[derive(Clone, Copy)]
struct Span {
    first: u64,
    last: u64,
}

fn linear(address: VirtAddr) -> u64 {
    address.as_u64() & ((1 << 48) - 1)
}

/// Bytes covered by one entry of a table of `level`
fn entry_size(level: u8) -> u64 {
    PAGE_SIZE << (9 * (level as u64 - 1))
}

fn prune_level(table: &mut PageTable, level: u8, base: u64, span: Span, phys_offset: VirtAddr, frames: &mut impl FrameSource) {
    let size = entry_size(level);
    for (index, entry) in table.iter_mut().enumerate() {
        let start = base + index as u64 * size;
        let end = start + size - 1;
        if end < span.first || start > span.last {
            continue;
        }
        if entry.is_unused() || entry.flags().contains(PageTableFlags::HUGE_PAGE) {
            continue;
        }
        // the children of a level 2 table are level 1 tables, which only hold pages
        if level > 2 {
            let child = unsafe { table_at(phys_offset, entry.addr()) };
            prune_level(child, level - 1, start, span, phys_offset, frames);
        }
        let shared = level == 4 && start >= USER_END;
        let frame = PhysFrame::containing_address(entry.addr());
        let empty = unsafe { table_at(phys_offset, entry.addr()) }.iter().all(|e| e.is_unused());
        if empty && !shared && frames.can_free(frame) {
            entry.set_unused();
            unsafe { frames.deallocate_frame(frame) };
        }
    }
}

/// # Safety
/// `address` must be a page table reachable through `phys_offset`, not referenced elsewhere.
pub unsafe fn table_at<'a>(phys_offset: VirtAddr, address: PhysAddr) -> &'a mut PageTable {
    unsafe { &mut *(phys_offset + address.as_u64()).as_mut_ptr::<PageTable>() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::boxed::Box;
    use alloc::collections::BTreeSet;
    use alloc::vec::Vec;
    use x86_64::structures::paging::{FrameDeallocator, Translate};

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
        assert_eq!(RegionFrameAllocator::count_frames(regions), frames.len());
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

    /// Frames backed by real host memory, so page tables built in them can be walked.
    /// Freed frames are only counted, never reused.
    struct HostFrames {
        handed_out: usize,
        limit: usize,
        freed: usize,
    }

    impl HostFrames {
        fn new(limit: usize) -> HostFrames {
            HostFrames { handed_out: 0, limit, freed: 0 }
        }
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

    impl FrameDeallocator<Size4KiB> for HostFrames {
        unsafe fn deallocate_frame(&mut self, _frame: PhysFrame) {
            self.freed += 1;
        }
    }

    impl FrameSource for HostFrames {
        fn used_frames(&self) -> usize {
            self.handed_out - self.freed
        }

        fn can_free(&self, _frame: PhysFrame) -> bool {
            true
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
        let mut frames = HostFrames::new(100);
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
        let mut frames = HostFrames::new(0);
        map_range(&mut mapper, &mut frames, VirtAddr::new(HEAP_START), 0, PageTableFlags::PRESENT).unwrap();
    }

    #[test]
    fn map_range_reports_overlap() {
        let mut mapper = host_mapper();
        let mut frames = HostFrames::new(100);
        let start = VirtAddr::new(0x40_0000);
        let user = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
        map_range(&mut mapper, &mut frames, start, PAGE_SIZE, user).unwrap();
        let error = map_range(&mut mapper, &mut frames, start, PAGE_SIZE, user).unwrap_err();
        assert!(matches!(error, MapRangeError::Map(MapToError::PageAlreadyMapped(_))), "{error:?}");
    }

    #[test]
    fn map_range_reports_out_of_frames() {
        let mut mapper = host_mapper();
        let mut frames = HostFrames::new(3);
        let user = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
        let error = map_range(&mut mapper, &mut frames, VirtAddr::new(0x40_0000), 4 * PAGE_SIZE, user).unwrap_err();
        assert!(matches!(error, MapRangeError::Map(MapToError::FrameAllocationFailed)), "{error:?}");
        assert_eq!(frames.used_frames(), 0, "rolled back");
        assert!((0..4).all(|page| mapper.translate_addr(VirtAddr::new(0x40_0000 + page * PAGE_SIZE)).is_none()));
        assert!(mapper.level_4_table()[0].is_unused(), "the tables are freed too");
    }

    #[test]
    fn map_range_of_whole_pages_maps_no_extra_page() {
        let mut mapper = host_mapper();
        let mut frames = HostFrames::new(100);
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
        let mut frames = HostFrames::new(100);
        let start = VirtAddr::new(HEAP_START + PAGE_SIZE - 1);
        map_range(&mut mapper, &mut frames, start, 2, PageTableFlags::PRESENT).unwrap();
        assert!(mapper.translate_addr(VirtAddr::new(HEAP_START)).is_some());
        assert!(mapper.translate_addr(VirtAddr::new(HEAP_START + PAGE_SIZE)).is_some());
        assert!(mapper.translate_addr(VirtAddr::new(HEAP_START + 2 * PAGE_SIZE)).is_none());
    }

    /// The page right below the non-canonical hole is the end of a range, not a step into it
    #[test]
    fn map_range_up_to_the_end_of_the_lower_half() {
        let mut mapper = host_mapper();
        let mut frames = HostFrames::new(100);
        let user = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
        let start = VirtAddr::new(USER_END - 2 * PAGE_SIZE);
        map_range(&mut mapper, &mut frames, start, 2 * PAGE_SIZE, user).unwrap();
        assert!(mapper.translate_addr(VirtAddr::new(USER_END - 1)).is_some());
        assert_eq!(unmap_range(&mut mapper, &mut frames, start, 2 * PAGE_SIZE).unwrap(), 2);
        assert_eq!(frames.used_frames(), 0);
    }

    #[test]
    fn unmapping_a_huge_range_skips_the_holes() {
        let mut mapper = host_mapper();
        let mut frames = HostFrames::new(100);
        let user = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
        map_range(&mut mapper, &mut frames, VirtAddr::new(0x40_0000), PAGE_SIZE, user).unwrap();
        map_range(&mut mapper, &mut frames, VirtAddr::new(USER_END - PAGE_SIZE), PAGE_SIZE, user).unwrap();
        // 2^35 pages: page by page this would never finish
        assert_eq!(unmap_range(&mut mapper, &mut frames, VirtAddr::new(0), USER_END).unwrap(), 2);
        assert_eq!(frames.used_frames(), 0);
        assert!(mapper.level_4_table().iter().all(|entry| entry.is_unused()));
    }

    #[test]
    fn map_range_refuses_invalid_ranges() {
        let mut mapper = host_mapper();
        let mut frames = HostFrames::new(100);
        let flags = PageTableFlags::PRESENT;
        let crossing_hole = map_range(&mut mapper, &mut frames, VirtAddr::new(USER_END - PAGE_SIZE), 2 * PAGE_SIZE, flags);
        assert!(matches!(crossing_hole, Err(MapRangeError::InvalidRange)));
        let past_the_end = map_range(&mut mapper, &mut frames, VirtAddr::new(u64::MAX - PAGE_SIZE + 1), 2 * PAGE_SIZE, flags);
        assert!(matches!(past_the_end, Err(MapRangeError::InvalidRange)));
        assert_eq!(frames.handed_out, 0, "nothing is mapped for a bad range");
    }

    /// Nikka 2-mm-5 `kernel_and_user_are_separated`
    #[test]
    fn kernel_and_user_are_separated() {
        let mut mapper = host_mapper();
        let mut frames = HostFrames::new(100);
        let kernel = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
        let user = kernel | PageTableFlags::USER_ACCESSIBLE;
        let (user_page, kernel_page) = (VirtAddr::new(0x40_0000), VirtAddr::new(HEAP_START));
        for (start, flags) in [(kernel_page, user), (user_page, kernel), (VirtAddr::new(USER_END - PAGE_SIZE), kernel)] {
            let result = map_range(&mut mapper, &mut frames, start, PAGE_SIZE, flags);
            assert!(matches!(result, Err(MapRangeError::PermissionDenied)), "{start:?} {flags:?}");
        }
        assert_eq!(frames.handed_out, 0, "nothing mapped for refused requests");
        map_range(&mut mapper, &mut frames, user_page, PAGE_SIZE, user).unwrap();
        map_range(&mut mapper, &mut frames, kernel_page, PAGE_SIZE, kernel).unwrap();
    }

    /// Nikka 2-mm-4 `no_excessive_intermediate_flags`: parent entries only get the
    /// flags that make sense on all levels, NX on a parent would block the whole subtree
    #[test]
    fn intermediate_entries_get_only_access_flags() {
        let mut mapper = host_mapper();
        let mut frames = HostFrames::new(100);
        let flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::NO_EXECUTE
            | PageTableFlags::GLOBAL
            | PageTableFlags::NO_CACHE;
        let start = VirtAddr::new(HEAP_START);
        map_range(&mut mapper, &mut frames, start, PAGE_SIZE, flags).unwrap();
        let level_4_entry = &mapper.level_4_table()[start.p4_index()];
        assert_eq!(level_4_entry.flags(), PageTableFlags::PRESENT | PageTableFlags::WRITABLE);
    }

    /// Nikka 2-mm-4 `check_map_intermediate`: a second mapping under the same parents
    /// reuses the tables. Tables of the lower half allow everything from the start: the
    /// rights of a page are in its leaf, so `mprotect` can widen them.
    #[test]
    fn intermediate_entries_are_reused_and_allow_everything() {
        let mut mapper = host_mapper();
        let mut frames = HostFrames::new(100);
        let read_only = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::NO_EXECUTE;
        let start = VirtAddr::new(0x40_0000);
        map_range(&mut mapper, &mut frames, start, PAGE_SIZE, read_only).unwrap();
        assert_eq!(mapper.level_4_table()[start.p4_index()].flags(), USER_TABLE_FLAGS);
        let tables_and_frame = frames.handed_out;
        map_range(&mut mapper, &mut frames, start + PAGE_SIZE, PAGE_SIZE, read_only | PageTableFlags::WRITABLE)
            .unwrap();
        assert_eq!(frames.handed_out, tables_and_frame + 1, "only a data frame, the tables are shared");
        let level_3 = unsafe { table_at(mapper.phys_offset(), mapper.level_4_table()[start.p4_index()].addr()) };
        let level_2 = unsafe { table_at(mapper.phys_offset(), level_3[start.p3_index()].addr()) };
        let level_1 = unsafe { table_at(mapper.phys_offset(), level_2[start.p2_index()].addr()) };
        assert_eq!(level_3[start.p3_index()].flags(), USER_TABLE_FLAGS);
        assert_eq!(level_2[start.p2_index()].flags(), USER_TABLE_FLAGS);
        assert_eq!(level_1[start.p1_index()].flags(), read_only, "the leaf has the page's own rights");
    }

    /// `protect_range` changes the leaves of pages with a frame and keeps the frame, also
    /// without `PRESENT`; `unmap_range` still frees such a frame
    #[test]
    fn protect_keeps_frames() {
        let mut mapper = host_mapper();
        let mut frames = HostFrames::new(100);
        let rw = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::WRITABLE;
        let start = VirtAddr::new(0x40_0000);
        map_range(&mut mapper, &mut frames, start, 2 * PAGE_SIZE, rw).unwrap();
        let frame = mapper.translate_addr(start).unwrap();
        let none = PageTableFlags::BIT_9;
        assert_eq!(protect_range(&mut mapper, start, 8 * PAGE_SIZE, none).unwrap(), 2, "pages without a frame are skipped");
        assert_eq!(mapper.translate_addr(start), Some(frame), "the frame is still there");
        match mapper.translate(start) {
            x86_64::structures::paging::mapper::TranslateResult::Mapped { flags, .. } => assert_eq!(flags, none),
            other => panic!("{other:?}"),
        }
        assert_eq!(protect_range(&mut mapper, start, PAGE_SIZE, rw).unwrap(), 1);
        assert_eq!(mapper.translate_addr(start), Some(frame));
        // a huge range walks only the tables there are
        assert_eq!(protect_range(&mut mapper, VirtAddr::new(0), USER_END, rw).unwrap(), 2);
        let before = frames.freed;
        assert_eq!(unmap_range(&mut mapper, &mut frames, start, 2 * PAGE_SIZE).unwrap(), 2);
        assert_eq!(frames.freed, before + 2 + 3, "both pages, the not present one too, and the tables");
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
