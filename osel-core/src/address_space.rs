//! Address spaces of programs
//!
//! Every address space has its own level 4 table. The lower half is the program's, the
//! upper half is the kernel's: its 256 level 4 entries are copied from the kernel table
//! and point to level 3 tables shared by all address spaces. The kernel creates all of
//! them at boot, so a kernel mapping made later shows up everywhere.

use crate::frames::FrameSource;
use crate::memory::{self, MapRangeError, USER_END, table_at};
use core::ops::Range;
use x86_64::structures::paging::{FrameAllocator, OffsetPageTable, PageTable, PageTableFlags, PhysFrame, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};

/// Level 4 entries of the kernel half, shared by every address space
pub const KERNEL_ENTRIES: Range<usize> = 256..512;

/// Flags of the level 4 entries the kernel creates for the upper half: the same as the
/// bootloader's, and never widened later, since kernel pages are never user accessible
pub const KERNEL_ENTRY_FLAGS: PageTableFlags = PageTableFlags::PRESENT.union(PageTableFlags::WRITABLE);

/// Points every unused kernel half entry of `level_4` to a fresh empty level 3 table.
/// Returns the number of tables created, `None` if the frames ran out.
pub fn create_kernel_tables(
    level_4: &mut PageTable,
    frames: &mut impl FrameAllocator<Size4KiB>,
    phys_offset: VirtAddr,
) -> Option<usize> {
    let mut created = 0;
    for entry in level_4.iter_mut().skip(KERNEL_ENTRIES.start) {
        if entry.is_unused() {
            let frame = frames.allocate_frame()?;
            unsafe { table_at(phys_offset, frame.start_address()) }.zero();
            entry.set_frame(frame, KERNEL_ENTRY_FLAGS);
            created += 1;
        }
    }
    Some(created)
}

/// A level 4 table with a private lower half. Has no `Drop`: it needs the frame source
/// to give its frames back, see `destroy`.
pub struct AddressSpace {
    level_4: PhysFrame,
    phys_offset: VirtAddr,
}

impl AddressSpace {
    /// An empty lower half and the kernel half of `kernel_level_4`. `None` if there is
    /// no frame for the table.
    pub fn new(kernel_level_4: &PageTable, frames: &mut impl FrameSource, phys_offset: VirtAddr) -> Option<Self> {
        let level_4 = frames.allocate_frame()?;
        let table = unsafe { table_at(phys_offset, level_4.start_address()) };
        table.zero();
        for index in KERNEL_ENTRIES {
            table[index] = kernel_level_4[index].clone();
        }
        Some(AddressSpace { level_4, phys_offset })
    }

    /// The frame to load into CR3
    pub fn level_4_frame(&self) -> PhysFrame {
        self.level_4
    }

    /// The page table of this space. Its upper half is shared with the kernel table:
    /// change it only through the kernel mapper.
    pub fn mapper(&mut self) -> OffsetPageTable<'_> {
        let table = unsafe { table_at(self.phys_offset, self.level_4.start_address()) };
        unsafe { OffsetPageTable::new(table, self.phys_offset) }
    }

    /// `memory::map_range` restricted to user pages of this space
    pub fn map(&mut self, frames: &mut impl FrameSource, start: VirtAddr, size: u64, flags: PageTableFlags) -> Result<(), MapRangeError> {
        if !flags.contains(PageTableFlags::USER_ACCESSIBLE) {
            return Err(MapRangeError::PermissionDenied);
        }
        memory::map_range(&mut self.mapper(), frames, start, size, flags)
    }

    /// `memory::unmap_range` restricted to the lower half
    pub fn unmap(&mut self, frames: &mut impl FrameSource, start: VirtAddr, size: u64) -> Result<usize, MapRangeError> {
        if start.as_u64() >= USER_END {
            return Err(MapRangeError::PermissionDenied);
        }
        memory::unmap_range(&mut self.mapper(), frames, start, size)
    }

    /// `memory::protect_range` restricted to the lower half
    pub fn protect(&mut self, start: VirtAddr, size: u64, flags: PageTableFlags) -> Result<usize, MapRangeError> {
        if start.as_u64() >= USER_END {
            return Err(MapRangeError::PermissionDenied);
        }
        memory::protect_range(&mut self.mapper(), start, size, flags)
    }

    /// Frees every frame of the lower half, its page tables and the level 4 table.
    /// Children are freed before their table: a freed frame gets overwritten.
    ///
    /// # Safety
    /// The space must not be active, and nothing may use its pages anymore.
    pub unsafe fn destroy(self, frames: &mut impl FrameSource) {
        let table = unsafe { table_at(self.phys_offset, self.level_4.start_address()) };
        for entry in table.iter().take(KERNEL_ENTRIES.start) {
            if !entry.is_unused() {
                unsafe { free_subtree(entry.addr(), 3, self.phys_offset, frames) };
            }
        }
        unsafe { frames.deallocate_frame(self.level_4) };
    }
}

/// Frees the table of `level` at `address` with everything below it
unsafe fn free_subtree(address: PhysAddr, level: u8, phys_offset: VirtAddr, frames: &mut impl FrameSource) {
    let table = unsafe { table_at(phys_offset, address) };
    for entry in table.iter() {
        if entry.is_unused() {
            continue;
        }
        assert!(!entry.flags().contains(PageTableFlags::HUGE_PAGE), "huge page in a user address space");
        if level == 1 {
            unsafe { frames.deallocate_frame(PhysFrame::containing_address(entry.addr())) };
        } else {
            unsafe { free_subtree(entry.addr(), level - 1, phys_offset, frames) };
        }
    }
    unsafe { frames.deallocate_frame(PhysFrame::containing_address(address)) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frames::FramePool;
    use crate::memory::{PAGE_SIZE, prune_empty_tables, unmap_range};
    use alloc::collections::BTreeSet;
    use alloc::vec;
    use alloc::vec::Vec;
    use bootloader_api::info::{MemoryRegion, MemoryRegionKind};
    use std::alloc::{Layout, alloc, dealloc};
    use x86_64::structures::paging::mapper::{MapToError, TranslateResult};
    use x86_64::structures::paging::{FrameDeallocator, Page, Translate};

    const MIB: u64 = 1024 * 1024;
    const FRAMES: u64 = 256;
    const USER: PageTableFlags = PageTableFlags::PRESENT.union(PageTableFlags::USER_ACCESSIBLE);
    const USER_RW: PageTableFlags = USER.union(PageTableFlags::WRITABLE);
    /// Kernel half entries the test kernel table has, like the boot code creates them
    const KERNEL_SLOTS: [usize; 3] = [256, 300, 511];

    /// Host memory standing in for physical `MIB..MIB + FRAMES * PAGE_SIZE`, filled with
    /// garbage like frames that come back from the pool
    struct Ram {
        memory: *mut u8,
        layout: Layout,
    }

    impl Ram {
        fn new() -> Ram {
            let layout = Layout::from_size_align((FRAMES * PAGE_SIZE) as usize, PAGE_SIZE as usize).unwrap();
            let memory = unsafe { alloc(layout) };
            assert!(!memory.is_null());
            unsafe { memory.write_bytes(0xff, layout.size()) };
            Ram { memory, layout }
        }

        fn phys_offset(&self) -> VirtAddr {
            VirtAddr::new(self.memory as u64 - MIB)
        }
    }

    impl Drop for Ram {
        fn drop(&mut self) {
            unsafe { dealloc(self.memory, self.layout) };
        }
    }

    /// A pool over the whole `Ram` and a kernel level 4 table with a few kernel half
    /// entries, the way the kernel has them after boot
    struct Machine<'a> {
        ram: &'a Ram,
        pool: FramePool<'a>,
        kernel: PhysFrame,
    }

    impl<'a> Machine<'a> {
        fn new(ram: &'a Ram, regions: &'a [MemoryRegion], bitmap: &'a mut [u64]) -> Machine<'a> {
            let mut pool = unsafe { FramePool::new(regions, ram.phys_offset().as_u64()) };
            pool.enable_tracking(bitmap);
            let kernel = pool.allocate_frame().unwrap();
            let table = unsafe { table_at(ram.phys_offset(), kernel.start_address()) };
            table.zero();
            for slot in KERNEL_SLOTS {
                let level_3 = pool.allocate_frame().unwrap();
                unsafe { table_at(ram.phys_offset(), level_3.start_address()) }.zero();
                table[slot].set_frame(level_3, KERNEL_ENTRY_FLAGS);
            }
            Machine { ram, pool, kernel }
        }

        fn kernel_table(&self) -> &'a mut PageTable {
            unsafe { table_at(self.ram.phys_offset(), self.kernel.start_address()) }
        }

        fn kernel_mapper(&self) -> OffsetPageTable<'a> {
            unsafe { OffsetPageTable::new(self.kernel_table(), self.ram.phys_offset()) }
        }

        fn new_space(&mut self) -> Option<AddressSpace> {
            AddressSpace::new(self.kernel_table(), &mut self.pool, self.ram.phys_offset())
        }

        fn read(&self, space: &mut AddressSpace, address: u64) -> u64 {
            let phys = space.mapper().translate_addr(VirtAddr::new(address)).expect("not mapped");
            unsafe { (self.ram.phys_offset() + phys.as_u64()).as_ptr::<u64>().read() }
        }

        fn write(&self, space: &mut AddressSpace, address: u64, value: u64) {
            let phys = space.mapper().translate_addr(VirtAddr::new(address)).expect("not mapped");
            unsafe { (self.ram.phys_offset() + phys.as_u64()).as_mut_ptr::<u64>().write(value) };
        }
    }

    fn with_machine(test: impl FnOnce(&mut Machine)) {
        let ram = Ram::new();
        let regions = [MemoryRegion { start: MIB, end: MIB + FRAMES * PAGE_SIZE, kind: MemoryRegionKind::Usable }];
        let mut bitmap = vec![0; FRAMES.div_ceil(64) as usize];
        let mut machine = Machine::new(&ram, &regions, &mut bitmap);
        test(&mut machine);
    }

    fn flags_of(space: &mut AddressSpace, address: u64) -> Option<PageTableFlags> {
        match space.mapper().translate(VirtAddr::new(address)) {
            TranslateResult::Mapped { flags, .. } => Some(flags),
            _ => None,
        }
    }

    #[test]
    fn a_new_space_shares_the_kernel_half_only() {
        with_machine(|machine| {
            let before = machine.pool.used_frames();
            let mut space = machine.new_space().unwrap();
            assert_eq!(machine.pool.used_frames(), before + 1, "only the level 4 table");
            let kernel = machine.kernel_table();
            let table = space.mapper();
            for index in 0..512 {
                if KERNEL_ENTRIES.contains(&index) {
                    assert_eq!(table.level_4_table()[index].addr(), kernel[index].addr(), "{index}");
                    assert_eq!(table.level_4_table()[index].flags(), kernel[index].flags(), "{index}");
                } else {
                    assert!(table.level_4_table()[index].is_unused(), "garbage in the lower half at {index}");
                }
            }
            unsafe { space.destroy(&mut machine.pool) };
            assert_eq!(machine.pool.used_frames(), before);
        });
    }

    #[test]
    fn mapped_user_pages_are_zeroed() {
        with_machine(|machine| {
            let mut space = machine.new_space().unwrap();
            // the frames hold 0xff garbage, and a freed frame also holds a list link
            let scrap = machine.pool.allocate_frame().unwrap();
            unsafe { machine.pool.deallocate_frame(scrap) };
            space.map(&mut machine.pool, VirtAddr::new(0x40_0000), 3 * PAGE_SIZE, USER_RW).unwrap();
            for offset in (0..3 * PAGE_SIZE).step_by(8) {
                assert_eq!(machine.read(&mut space, 0x40_0000 + offset), 0, "offset {offset:#x}");
            }
            assert_eq!(flags_of(&mut space, 0x40_0000), Some(USER_RW));
            unsafe { space.destroy(&mut machine.pool) };
        });
    }

    #[test]
    fn spaces_do_not_see_each_others_pages() {
        with_machine(|machine| {
            let mut a = machine.new_space().unwrap();
            let mut b = machine.new_space().unwrap();
            a.map(&mut machine.pool, VirtAddr::new(0x40_0000), PAGE_SIZE, USER_RW).unwrap();
            machine.write(&mut a, 0x40_0000, 7);
            assert!(b.mapper().translate_addr(VirtAddr::new(0x40_0000)).is_none());
            b.map(&mut machine.pool, VirtAddr::new(0x40_0000), PAGE_SIZE, USER_RW).unwrap();
            assert_eq!(machine.read(&mut b, 0x40_0000), 0);
            assert_eq!(machine.read(&mut a, 0x40_0000), 7);
            unsafe { a.destroy(&mut machine.pool) };
            assert_eq!(machine.read(&mut b, 0x40_0000), 0, "b keeps its page");
            unsafe { b.destroy(&mut machine.pool) };
        });
    }

    /// A kernel mapping made after the space was created is visible in it
    #[test]
    fn the_kernel_half_is_shared() {
        with_machine(|machine| {
            let mut space = machine.new_space().unwrap();
            let address = VirtAddr::new_truncate(300 << 39);
            let kernel_flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
            memory::map_range(&mut machine.kernel_mapper(), &mut machine.pool, address, PAGE_SIZE, kernel_flags).unwrap();
            let in_kernel = machine.kernel_mapper().translate_addr(address);
            assert!(in_kernel.is_some());
            assert_eq!(space.mapper().translate_addr(address), in_kernel);
            unmap_range(&mut machine.kernel_mapper(), &mut machine.pool, address, PAGE_SIZE).unwrap();
            assert!(space.mapper().translate_addr(address).is_none());
            unsafe { space.destroy(&mut machine.pool) };
        });
    }

    #[test]
    fn a_space_maps_only_user_pages() {
        with_machine(|machine| {
            let mut space = machine.new_space().unwrap();
            let used = machine.pool.used_frames();
            let kernel_flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
            let kernel = VirtAddr::new_truncate(300 << 39);
            for (start, flags) in [(VirtAddr::new(0x40_0000), kernel_flags), (kernel, kernel_flags), (kernel, USER_RW)] {
                let result = space.map(&mut machine.pool, start, PAGE_SIZE, flags);
                assert!(matches!(result, Err(MapRangeError::PermissionDenied)), "{start:?} {flags:?}");
            }
            assert!(matches!(space.unmap(&mut machine.pool, kernel, PAGE_SIZE), Err(MapRangeError::PermissionDenied)));
            let global = space.map(&mut machine.pool, VirtAddr::new(0x40_0000), PAGE_SIZE, USER | PageTableFlags::GLOBAL);
            assert!(matches!(global, Err(MapRangeError::PermissionDenied)), "a global user page");
            assert_eq!(machine.pool.used_frames(), used);
            unsafe { space.destroy(&mut machine.pool) };
        });
    }

    /// Nikka 2-mm-5 `map_twice`: mapping over a mapped page fails in the middle of the
    /// range, the pages mapped before it are rolled back, the old page keeps its data
    #[test]
    fn mapping_over_a_mapped_page_rolls_back() {
        with_machine(|machine| {
            let mut space = machine.new_space().unwrap();
            space.map(&mut machine.pool, VirtAddr::new(0x40_3000), PAGE_SIZE, USER_RW).unwrap();
            machine.write(&mut space, 0x40_3000, 42);
            let used = machine.pool.used_frames();
            let result = space.map(&mut machine.pool, VirtAddr::new(0x40_0000), 8 * PAGE_SIZE, USER_RW);
            assert!(matches!(result, Err(MapRangeError::Map(MapToError::PageAlreadyMapped(_)))), "{:?}", result.err());
            assert_eq!(machine.pool.used_frames(), used, "nothing left from the failed call");
            for page in [0x40_0000, 0x40_1000, 0x40_2000, 0x40_4000] {
                assert!(flags_of(&mut space, page).is_none(), "{page:#x} still mapped");
            }
            assert_eq!(machine.read(&mut space, 0x40_3000), 42);
            unsafe { space.destroy(&mut machine.pool) };
        });
    }

    /// Nikka 2-mm-6 `duplicate_error`: running out of frames at every step of creating a
    /// space and mapping a range that needs new tables on all levels leaks nothing
    #[test]
    fn running_out_of_frames_at_every_step_leaks_nothing() {
        // crosses a level 1, a level 2 and a level 3 boundary
        let start = VirtAddr::new((1 << 39) - 2 * PAGE_SIZE);
        let size = 4 * PAGE_SIZE;
        with_machine(|machine| {
            let used = machine.pool.used_frames();
            let mut space = machine.new_space().unwrap();
            space.map(&mut machine.pool, start, size, USER_RW).unwrap();
            let needed = machine.pool.used_frames() - used;
            unsafe { space.destroy(&mut machine.pool) };
            assert_eq!(machine.pool.used_frames(), used);
            // 1 level 4 + 2 level 3 + 2 level 2 + 2 level 1 tables + 4 pages
            assert_eq!(needed, 11);

            for available in 0..needed {
                let mut stolen = Vec::new();
                while let Some(frame) = machine.pool.allocate_frame() {
                    stolen.push(frame);
                }
                for frame in stolen.drain(stolen.len() - available..) {
                    unsafe { machine.pool.deallocate_frame(frame) };
                }
                let before = machine.pool.used_frames();
                match machine.new_space() {
                    None => assert_eq!(available, 0),
                    Some(mut space) => {
                        let result = space.map(&mut machine.pool, start, size, USER_RW);
                        assert!(matches!(result, Err(MapRangeError::Map(MapToError::FrameAllocationFailed))), "{available}");
                        assert!(space.mapper().level_4_table().iter().take(256).all(|e| e.is_unused()), "{available}");
                        unsafe { space.destroy(&mut machine.pool) };
                    }
                }
                assert_eq!(machine.pool.used_frames(), before, "leak with {available} free frames");
                for frame in stolen {
                    unsafe { machine.pool.deallocate_frame(frame) };
                }
            }
        });
    }

    #[test]
    fn destroy_frees_everything_of_the_lower_half() {
        with_machine(|machine| {
            let used = machine.pool.used_frames();
            let mut space = machine.new_space().unwrap();
            let rx = USER;
            let r_nx = USER | PageTableFlags::NO_EXECUTE;
            for (start, flags) in [(0x40_0000u64, rx), (0x60_0000, r_nx), (0x7f_0000_0000, USER_RW), (1 << 46, USER_RW)] {
                space.map(&mut machine.pool, VirtAddr::new(start), 2 * PAGE_SIZE, flags).unwrap();
                assert_eq!(flags_of(&mut space, start), Some(flags));
            }
            let kernel_entries: Vec<_> = machine.kernel_table().iter().map(|e| (e.addr(), e.flags())).collect();
            unsafe { space.destroy(&mut machine.pool) };
            assert_eq!(machine.pool.used_frames(), used);
            let after: Vec<_> = machine.kernel_table().iter().map(|e| (e.addr(), e.flags())).collect();
            assert_eq!(after, kernel_entries, "the kernel table is untouched");
            for slot in KERNEL_SLOTS {
                let level_3 = PhysFrame::containing_address(machine.kernel_table()[slot].addr());
                assert!(machine.pool.can_free(level_3), "shared table {slot} was freed");
            }
        });
    }

    /// Nikka 2-mm-7 `drop_subtree`: unmapping frees exactly the tables that became empty
    #[test]
    fn unmapping_frees_the_emptied_tables() {
        with_machine(|machine| {
            let mut space = machine.new_space().unwrap();
            let used = machine.pool.used_frames();
            // two pages under one level 1 table, one page under another level 2 entry
            space.map(&mut machine.pool, VirtAddr::new(0x40_0000), 2 * PAGE_SIZE, USER_RW).unwrap();
            space.map(&mut machine.pool, VirtAddr::new(0x4000_0000), PAGE_SIZE, USER_RW).unwrap();
            assert_eq!(machine.pool.used_frames(), used + 3 + 3 + 2);
            assert_eq!(space.unmap(&mut machine.pool, VirtAddr::new(0x40_0000), PAGE_SIZE).unwrap(), 1);
            assert_eq!(machine.pool.used_frames(), used + 3 + 2 + 2, "the level 1 table still holds a page");
            assert_eq!(space.unmap(&mut machine.pool, VirtAddr::new(0x40_0000), 2 * PAGE_SIZE).unwrap(), 1);
            assert_eq!(machine.pool.used_frames(), used + 4, "its level 1 and level 2 tables are gone");
            assert_eq!(space.unmap(&mut machine.pool, VirtAddr::new(0x40_0000), 2 * PAGE_SIZE).unwrap(), 0, "nothing left");
            space.unmap(&mut machine.pool, VirtAddr::new(0x4000_0000), PAGE_SIZE).unwrap();
            assert_eq!(machine.pool.used_frames(), used);
            assert!(space.mapper().level_4_table()[0].is_unused());
            unsafe { space.destroy(&mut machine.pool) };
        });
    }

    #[test]
    fn pruning_keeps_the_shared_kernel_tables() {
        with_machine(|machine| {
            let address = VirtAddr::new_truncate(256 << 39);
            let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
            let used = machine.pool.used_frames();
            memory::map_range(&mut machine.kernel_mapper(), &mut machine.pool, address, PAGE_SIZE, flags).unwrap();
            unmap_range(&mut machine.kernel_mapper(), &mut machine.pool, address, PAGE_SIZE).unwrap();
            assert_eq!(machine.pool.used_frames(), used, "level 2 and 1 tables freed");
            let last = Page::containing_address(VirtAddr::new(u64::MAX));
            prune_empty_tables(&mut machine.kernel_mapper(), &mut machine.pool, Page::containing_address(address), last);
            for slot in KERNEL_SLOTS {
                assert!(!machine.kernel_table()[slot].is_unused(), "shared level 3 table {slot} was freed");
            }
        });
    }

    /// Random maps and unmaps against a model of mapped pages, then destroy leaks nothing
    #[test]
    fn random_operations_match_a_model() {
        with_machine(|machine| {
            let used = machine.pool.used_frames();
            let mut space = machine.new_space().unwrap();
            let mut seed = 0x2545_f491_4f6c_dd1du64;
            let mut random = move |limit: u64| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed % limit
            };
            // pages spread over two level 1 tables and two level 4 entries
            let slots = [0x40_0000u64, 0x80_0000, 1 << 39];
            let mut mapped = BTreeSet::new();
            for _ in 0..2000 {
                let start = slots[random(3) as usize] + random(16) * PAGE_SIZE;
                let pages = random(4) + 1;
                let range: Vec<u64> = (0..pages).map(|i| start + i * PAGE_SIZE).collect();
                if random(2) == 0 {
                    let result = space.map(&mut machine.pool, VirtAddr::new(start), pages * PAGE_SIZE, USER_RW);
                    let free = range.iter().all(|page| !mapped.contains(page));
                    match result {
                        Ok(()) => {
                            assert!(free);
                            mapped.extend(range);
                        }
                        Err(MapRangeError::Map(MapToError::PageAlreadyMapped(_))) => assert!(!free),
                        Err(error) => panic!("{error:?}"),
                    }
                } else {
                    let unmapped = space.unmap(&mut machine.pool, VirtAddr::new(start), pages * PAGE_SIZE).unwrap();
                    let expected = range.iter().filter(|page| mapped.remove(*page)).count();
                    assert_eq!(unmapped, expected);
                }
                for slot in slots {
                    for page in (slot..slot + 20 * PAGE_SIZE).step_by(PAGE_SIZE as usize) {
                        assert_eq!(flags_of(&mut space, page).is_some(), mapped.contains(&page), "{page:#x}");
                    }
                }
            }
            unsafe { space.destroy(&mut machine.pool) };
            assert_eq!(machine.pool.used_frames(), used);
        });
    }
}
