//! memory module manages page tables and physical frames
//!
//! The address space layout and the frame allocator live in `osel_core::memory`.

use alloc::vec;
use bootloader_api::info::MemoryRegion;
use crate::sync::IrqSpinlock;
use osel_core::errno::{Errno, KResult};
use osel_core::mmap::{Mappings, PageTables, Touch};
use osel_core::user_mem::{self, Access};
use x86_64::VirtAddr;
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::{Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame, Size4KiB};

pub use osel_core::address_space::AddressSpace;
pub use osel_core::frames::{FramePool, FrameSource};
pub use osel_core::memory::*;

pub struct Memory {
    pub mapper: OffsetPageTable<'static>,
    pub frame_allocator: FramePool<'static>,
    /// Level 4 table of the kernel, active whenever no program runs
    pub kernel_level_4: PhysFrame,
}

static MEMORY: IrqSpinlock<Option<Memory>> = IrqSpinlock::new("MEMORY", None);

/// Level 4 entries of the lower half the bootloader left behind, cleared by `init`
static mut LOWER_HALF_ENTRIES_CLEARED: usize = 0;

/// The memory map from the bootloader
static mut MEMORY_REGIONS: &[MemoryRegion] = &[];

/// Takes over the active page table and the usable memory reported by the bootloader
///
/// # Safety
/// The whole physical memory must be mapped at `physical_memory_offset`,
/// and the function must be called only once.
pub unsafe fn init(physical_memory_offset: VirtAddr, memory_regions: &'static [MemoryRegion]) {
    let kernel_level_4 = Cr3::read().0;
    let level_4_table = unsafe { active_level_4_table(physical_memory_offset) };
    let cleared = clear_lower_half(level_4_table);
    unsafe {
        LOWER_HALF_ENTRIES_CLEARED = cleared;
        MEMORY_REGIONS = memory_regions;
    }
    let mut mapper = unsafe { OffsetPageTable::new(level_4_table, physical_memory_offset) };
    unmap_interrupt_stack_guards(&mut mapper);
    let mut frame_allocator = unsafe { FramePool::new(memory_regions, physical_memory_offset.as_u64()) };
    // every address space copies the kernel half, so all of its level 3 tables exist from now on
    osel_core::address_space::create_kernel_tables(mapper.level_4_table_mut(), &mut frame_allocator, physical_memory_offset)
        .expect("no frames for the kernel page tables");
    *MEMORY.lock() = Some(Memory { mapper, frame_allocator, kernel_level_4 });
}

/// Lets frames be freed. The bitmap of free frames lives on the heap, so this runs
/// after `allocator::init_heap`; it is allocated before taking `MEMORY`, which must
/// never wait for `ALLOCATOR`.
pub fn enable_frame_tracking() {
    let words = with_memory(|memory| memory.frame_allocator.bitmap_words());
    let bitmap = vec![0u64; words].leak();
    with_memory(|memory| memory.frame_allocator.enable_tracking(bitmap));
}

/// Frames handed out and not freed yet, 0 before `init` (a test kernel may skip it)
pub fn used_frames() -> usize {
    MEMORY.lock().as_ref().map_or(0, |memory| memory.frame_allocator.used_frames())
}

/// Address space of a program: the lower half is its own, the upper half is the kernel's.
/// Dropping it frees all of its frames; it must not be active then.
pub struct UserSpace {
    space: Option<AddressSpace>,
    /// What the program may touch; pages are mapped on the first touch
    mappings: Mappings,
}

impl UserSpace {
    /// An empty address space, `None` if there is no frame for its table
    pub fn new() -> Option<UserSpace> {
        let space = with_memory(|m| AddressSpace::new(m.mapper.level_4_table(), &mut m.frame_allocator, m.mapper.phys_offset()))?;
        Some(UserSpace { space: Some(space), mappings: Mappings::new() })
    }

    /// Moves the address space out, leaving an empty `UserSpace` that owns nothing
    pub fn take(&mut self) -> UserSpace {
        UserSpace { space: self.space.take(), mappings: core::mem::take(&mut self.mappings) }
    }

    /// The level 4 table to load into CR3
    pub fn level_4_frame(&self) -> PhysFrame {
        self.space.as_ref().expect("address space is destroyed").level_4_frame()
    }

    fn space(&mut self) -> &mut AddressSpace {
        self.space.as_mut().expect("address space is destroyed")
    }

    /// Maps zeroed user pages right away, see `map_range`. They become an area the program
    /// may `munmap` or `mprotect`.
    pub fn map(&mut self, start: VirtAddr, size: u64, flags: PageTableFlags) -> Result<(), MapRangeError> {
        let active = self.is_active();
        let space = self.space();
        let result = with_memory(|m| space.map(&mut m.frame_allocator, start, size, flags));
        // the CPU may have cached the rolled back pages speculatively
        if result.is_err() && active {
            x86_64::instructions::tlb::flush_all();
        }
        if result.is_ok() && size > 0 {
            let (first, end) = page_span(start.as_u64(), size);
            self.mappings.record(first, end, flags);
        }
        result
    }

    /// Unmaps user pages and frees their frames, returns how many were mapped
    pub fn unmap(&mut self, start: VirtAddr, size: u64) -> Result<usize, MapRangeError> {
        let active = self.is_active();
        let space = self.space();
        let result = with_memory(|m| space.unmap(&mut m.frame_allocator, start, size));
        if active {
            x86_64::instructions::tlb::flush_all();
        }
        if result.is_ok() && size > 0 {
            let (first, end) = page_span(start.as_u64(), size);
            self.mappings.forget(first, end);
        }
        result
    }

    /// The areas and the page tables they change
    fn parts(&mut self) -> (&mut Mappings, Tables<'_>) {
        let active = self.is_active();
        let space = self.space.as_mut().expect("address space is destroyed");
        (&mut self.mappings, Tables { space, active })
    }

    /// The areas of the program
    pub fn mappings(&self) -> &Mappings {
        &self.mappings
    }

    /// `mmap` of anonymous memory, see `Mappings::mmap`
    pub fn mmap(&mut self, addr: u64, len: u64, prot: u32, flags: u64) -> KResult<u64> {
        let (mappings, mut tables) = self.parts();
        mappings.mmap(&mut tables, addr, len, prot, flags)
    }

    pub fn munmap(&mut self, addr: u64, len: u64) -> KResult<()> {
        let (mappings, mut tables) = self.parts();
        mappings.munmap(&mut tables, addr, len)
    }

    pub fn mprotect(&mut self, addr: u64, len: u64, prot: u32) -> KResult<()> {
        let (mappings, mut tables) = self.parts();
        mappings.mprotect(&mut tables, addr, len, prot)
    }

    pub fn madvise(&mut self, addr: u64, len: u64, advice: u64) -> KResult<()> {
        let (mappings, mut tables) = self.parts();
        mappings.madvise(&mut tables, addr, len, advice)
    }

    /// A page fault of the program at `address`: `true` if a page was mapped and the
    /// program goes on. Takes `MEMORY` and allocates nothing on the heap.
    pub fn fault(&mut self, address: u64, touch: Touch) -> bool {
        let (mappings, mut tables) = self.parts();
        mappings.fault(&mut tables, address, touch)
    }

    /// Reads memory a program handed to the kernel: every page must be the program's,
    /// present and user accessible, otherwise `EFAULT` and nothing is read
    pub fn copy_from_user(&mut self, address: u64, buffer: &mut [u8]) -> KResult<()> {
        self.check_user(address, buffer.len(), Access::Read)?;
        self.read_bytes(address, buffer)
    }

    /// Writes into memory a program handed to the kernel; the pages must also be writable
    pub fn copy_to_user(&mut self, address: u64, bytes: &[u8]) -> KResult<()> {
        self.check_user(address, bytes.len(), Access::Write)?;
        self.write_bytes(address, bytes)
    }

    /// Reads a NUL-terminated string a program handed to the kernel, at most `max` bytes
    /// with the NUL: page by page, so a short string just before an unmapped page is fine.
    /// `EFAULT` on memory that is not the program's, `ENAMETOOLONG` without a NUL in `max`.
    pub fn copy_string_from_user(&mut self, address: u64, max: usize) -> KResult<alloc::vec::Vec<u8>> {
        let mut bytes = alloc::vec::Vec::new();
        let mut at = address;
        while bytes.len() < max {
            let in_page = (PAGE_SIZE - at % PAGE_SIZE) as usize;
            let len = in_page.min(max - bytes.len());
            let start = bytes.len();
            bytes.resize(start + len, 0);
            self.copy_from_user(at, &mut bytes[start..])?;
            if let Some(nul) = bytes[start..].iter().position(|&b| b == 0) {
                bytes.truncate(start + nul);
                return Ok(bytes);
            }
            at += len as u64;
        }
        Err(Errno::ENAMETOOLONG)
    }

    /// Checks that `len` bytes at `address` are the program's memory, see `user_mem::check`.
    /// Untouched pages of the program's areas are mapped first, as its own touch would.
    pub fn check_user(&mut self, address: u64, len: usize, access: Access) -> KResult<()> {
        let touch = match access {
            Access::Read => Touch::Read,
            Access::Write => Touch::Write,
        };
        let (mappings, mut tables) = self.parts();
        mappings.fault_in(&mut tables, address, len as u64, touch);
        // the leaf flags are enough: every table of the lower half allows everything
        // (`USER_TABLE_FLAGS`)
        user_mem::check(address, len as u64, access, |page| self.page_flags(VirtAddr::new(page)))
    }

    /// Copies `bytes` to `address` in this space through the physical memory mapping,
    /// whatever the page flags. Only the lower half: `EFAULT` for the kernel half and at
    /// the first page that is not mapped.
    pub fn write_bytes(&mut self, address: u64, bytes: &[u8]) -> KResult<()> {
        self.copy(address, bytes.len(), |memory, done, len| unsafe {
            core::ptr::copy_nonoverlapping(bytes[done..].as_ptr(), memory, len)
        })
    }

    /// Copies from `address` in this space into `buffer`, like `write_bytes`
    pub fn read_bytes(&mut self, address: u64, buffer: &mut [u8]) -> KResult<()> {
        let len = buffer.len();
        self.copy(address, len, |memory, done, len| unsafe {
            core::ptr::copy_nonoverlapping(memory, buffer[done..].as_mut_ptr(), len)
        })
    }

    /// Calls `copy(kernel pointer, bytes done, chunk length)` for every page of the range
    fn copy(&mut self, address: u64, len: usize, mut copy: impl FnMut(*mut u8, usize, usize)) -> KResult<()> {
        let offset = with_memory(|m| m.mapper.phys_offset());
        let mut done = 0;
        while done < len {
            let at = address.checked_add(done as u64).ok_or(Errno::EFAULT)?;
            if at >= USER_END {
                return Err(Errno::EFAULT);
            }
            let virt = VirtAddr::new(at);
            let phys = self.translate(virt).ok_or(Errno::EFAULT)?;
            let chunk = (len - done).min((PAGE_SIZE - at % PAGE_SIZE) as usize);
            copy((offset + phys.as_u64()).as_mut_ptr(), done, chunk);
            done += chunk;
        }
        Ok(())
    }

    /// Flags of the page at `address`, `None` if it is not mapped
    pub fn page_flags(&mut self, address: VirtAddr) -> Option<PageTableFlags> {
        use x86_64::structures::paging::mapper::{Translate, TranslateResult};
        match self.space().mapper().translate(address) {
            TranslateResult::Mapped { flags, .. } => Some(flags),
            _ => None,
        }
    }

    /// Physical address `address` is mapped to in this space
    pub fn translate(&mut self, address: VirtAddr) -> Option<x86_64::PhysAddr> {
        use x86_64::structures::paging::Translate;
        self.space().mapper().translate_addr(address)
    }

    pub fn is_active(&self) -> bool {
        self.space.as_ref().is_some_and(|space| Cr3::read().0 == space.level_4_frame())
    }

    /// Switches to this address space. The kernel half stays the same, so kernel code,
    /// stacks and data keep working.
    pub fn activate(&mut self) {
        let frame = self.space().level_4_frame();
        unsafe { Cr3::write(frame, Cr3::read().1) };
    }
}

impl Drop for UserSpace {
    /// Takes `MEMORY`: dropping a space inside `with_memory` is a deadlock
    fn drop(&mut self) {
        assert!(!self.is_active(), "dropping the active address space");
        if let Some(space) = self.space.take() {
            with_memory(|m| unsafe { space.destroy(&mut m.frame_allocator) });
        }
    }
}

/// First page and end of the pages `size > 0` bytes at `start` touch
fn page_span(start: u64, size: u64) -> (u64, u64) {
    let first = start & !(PAGE_SIZE - 1);
    let end = start.saturating_add(size).saturating_add(PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    (first, end)
}

/// The page tables of a `UserSpace` for `Mappings`. Every change takes `MEMORY` by itself:
/// the areas allocate on the heap, which must not happen inside `with_memory`.
struct Tables<'a> {
    space: &'a mut AddressSpace,
    /// Whether the space is in CR3: then changes flush the TLB
    active: bool,
}

impl PageTables for Tables<'_> {
    fn has_frame(&mut self, page: u64) -> bool {
        use x86_64::structures::paging::mapper::{Translate, TranslateResult};
        // a not present entry with a frame (`PROT_NONE`) translates too
        matches!(self.space.mapper().translate(VirtAddr::new(page)), TranslateResult::Mapped { .. })
    }

    fn map_zeroed(&mut self, page: u64, flags: PageTableFlags) -> bool {
        // not present translations are never cached: nothing to flush
        with_memory(|m| self.space.map(&mut m.frame_allocator, VirtAddr::new(page), PAGE_SIZE, flags)).is_ok()
    }

    fn unmap(&mut self, start: u64, end: u64) {
        with_memory(|m| self.space.unmap(&mut m.frame_allocator, VirtAddr::new(start), end - start)).expect("an area is in the lower half");
        if self.active {
            x86_64::instructions::tlb::flush_all();
        }
    }

    fn protect(&mut self, start: u64, end: u64, flags: PageTableFlags) {
        with_memory(|_| self.space.protect(VirtAddr::new(start), end - start, flags)).expect("an area is in the lower half");
        // also when the rights grow: a stale read-only entry would fault for good
        if self.active {
            x86_64::instructions::tlb::flush_all();
        }
    }
}

/// Switches back to the kernel's own level 4 table
pub fn activate_kernel_space() {
    let frame = with_memory(|m| m.kernel_level_4);
    unsafe { Cr3::write(frame, Cr3::read().1) };
}

/// The bootloader identity maps a couple of its own pages (GDT, context switch code)
/// in the lower half. Nothing of the kernel lives there, and the lower half belongs
/// to user programs, so everything is removed. Returns the number of removed entries.
fn clear_lower_half(level_4_table: &mut PageTable) -> usize {
    let mut cleared = 0;
    for entry in level_4_table.iter_mut().take(256) {
        if !entry.is_unused() {
            entry.set_unused();
            cleared += 1;
        }
    }
    x86_64::instructions::tlb::flush_all();
    cleared
}

/// The interrupt stacks are kernel statics with a spare page below each: unmapping
/// that page turns a stack overflow in the panic path into a fault, not memory corruption
fn unmap_interrupt_stack_guards(mapper: &mut OffsetPageTable) {
    for stack in crate::gdt::ist_stacks() {
        let guard = Page::<Size4KiB>::containing_address(stack.guard);
        // the frame stays allocated to the kernel image, it is just not reachable anymore
        let (_, flush) = mapper.unmap(guard).expect("interrupt stack guard page is not a 4 KiB page");
        flush.flush();
    }
}

/// Number of frames the frame allocator can hand out in total, according to the memory map
pub fn total_usable_frames() -> usize {
    FramePool::count_frames(unsafe { MEMORY_REGIONS })
}

/// Runs `f` with exclusive access to the page table and the frame allocator
/// (and interrupts off). Nesting it is a deadlock, reported as a panic.
#[track_caller]
pub fn with_memory<R>(f: impl FnOnce(&mut Memory) -> R) -> R {
    f(MEMORY.lock().as_mut().expect("memory is not initialized"))
}

unsafe fn active_level_4_table(physical_memory_offset: VirtAddr) -> &'static mut PageTable {
    let (level_4_table_frame, _) = Cr3::read();
    let virt = physical_memory_offset + level_4_table_frame.start_address().as_u64();
    unsafe { &mut *virt.as_mut_ptr() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    use x86_64::structures::paging::{FrameAllocator, FrameDeallocator, Mapper, Page, PageTableFlags, Size4KiB, Translate};

    /// A free spot after the heap for test mappings
    const SCRATCH: u64 = HEAP_START + HEAP_SIZE + 0x10_0000;

    /// A kernel level 4 entry nothing uses: its level 3 table exists only since boot
    const FRESH_ENTRY: u64 = HEAP_START + (1 << 39);

    /// Frees the page tables test mappings created under `SCRATCH`
    fn free_scratch_tables(memory: &mut Memory) {
        let first = Page::containing_address(VirtAddr::new(SCRATCH));
        prune_empty_tables(&mut memory.mapper, &mut memory.frame_allocator, first, first + 15);
    }

    const USER_RW: PageTableFlags =
        PageTableFlags::PRESENT.union(PageTableFlags::WRITABLE).union(PageTableFlags::USER_ACCESSIBLE);

    #[test_case]
    fn test_every_kernel_entry_exists() {
        with_memory(|m| {
            for (index, entry) in m.mapper.level_4_table().iter().enumerate().skip(256) {
                let flags = entry.flags() - PageTableFlags::ACCESSED - PageTableFlags::DIRTY;
                assert_eq!(flags, PageTableFlags::PRESENT | PageTableFlags::WRITABLE, "level 4 entry {}", index);
            }
        });
    }

    /// Kernel pages are never user accessible, so `map_to` never widens a level 4 entry and
    /// the copies in address spaces stay equal to the kernel's
    #[test_case]
    fn test_no_kernel_entry_is_user_accessible() {
        fn check(table: &PageTable, level: u8, offset: VirtAddr) {
            for entry in table.iter().filter(|e| !e.is_unused()) {
                assert!(!entry.flags().contains(PageTableFlags::USER_ACCESSIBLE), "level {} entry {:?}", level, entry);
                if level > 1 && !entry.flags().contains(PageTableFlags::HUGE_PAGE) {
                    check(unsafe { &*(offset + entry.addr().as_u64()).as_ptr::<PageTable>() }, level - 1, offset);
                }
            }
        }
        with_memory(|m| {
            let offset = m.mapper.phys_offset();
            for entry in m.mapper.level_4_table().iter().skip(256) {
                check(unsafe { &*(offset + entry.addr().as_u64()).as_ptr::<PageTable>() }, 3, offset);
            }
        });
    }

    #[test_case]
    fn test_user_space_runs_kernel_code_and_its_own_pages() {
        let mut space = UserSpace::new().expect("out of frames");
        let page = VirtAddr::new(0x40_0000);
        space.map(page, 2 * 4096, USER_RW).unwrap();
        assert!(!space.is_active());
        space.activate();
        assert!(space.is_active());
        // the kernel half works: heap, statics, this stack
        let values: Vec<u64> = (0..100).collect();
        let pointer = page.as_mut_ptr::<u64>();
        unsafe {
            assert_eq!(pointer.read_volatile(), 0, "a fresh page is zeroed");
            pointer.write_volatile(values.iter().sum());
            assert_eq!(pointer.read_volatile(), 4950);
        }
        activate_kernel_space();
        assert!(!space.is_active());
        assert!(with_memory(|m| m.mapper.translate_addr(page)).is_none(), "the kernel table has no user pages");
        drop(values);
    }

    #[test_case]
    fn test_kernel_addresses_are_the_same_in_a_user_space() {
        static VALUE: u64 = 1;
        let local = 0u64;
        let heap = alloc::boxed::Box::new(0u64);
        let addresses = [
            VirtAddr::from_ptr(&VALUE),
            VirtAddr::from_ptr(&local),
            VirtAddr::from_ptr(&*heap),
            VirtAddr::new(activate_kernel_space as *const () as u64),
            with_memory(|m| m.mapper.phys_offset()) + 0x20_0000u64,
        ];
        let mut space = UserSpace::new().unwrap();
        for address in addresses {
            let in_kernel = with_memory(|m| m.mapper.translate_addr(address));
            assert!(in_kernel.is_some());
            assert_eq!(space.translate(address), in_kernel, "{:?}", address);
        }
    }

    /// The upper half is shared: a kernel mapping made after the space exists shows up in it
    #[test_case]
    fn test_later_kernel_mappings_reach_every_space() {
        let mut space = UserSpace::new().unwrap();
        let address = VirtAddr::new(FRESH_ENTRY);
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
        with_memory(|m| map_range(&mut m.mapper, &mut m.frame_allocator, address, 4096, flags)).unwrap();
        let in_kernel = with_memory(|m| m.mapper.translate_addr(address));
        assert!(in_kernel.is_some());
        assert_eq!(space.translate(address), in_kernel);
        with_memory(|m| unmap_range(&mut m.mapper, &mut m.frame_allocator, address, 4096)).unwrap();
        x86_64::instructions::tlb::flush(address);
        assert!(space.translate(address).is_none());
    }

    #[test_case]
    fn test_copy_from_and_to_user_checks_every_page() {
        let mut space = UserSpace::new().unwrap();
        let read_only = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
        space.map(VirtAddr::new(0x40_0000), 4096, USER_RW).unwrap();
        space.map(VirtAddr::new(0x40_1000), 4096, read_only).unwrap();
        space.copy_to_user(0x40_0ffc, b"abcd").unwrap();
        let mut buffer = [0u8; 8];
        space.copy_from_user(0x40_0ffc, &mut buffer).unwrap();
        assert_eq!(&buffer, b"abcd\0\0\0\0", "across into the read-only page");
        assert!(matches!(space.copy_to_user(0x40_0ffc, b"abcde"), Err(Errno::EFAULT)), "into the read-only page");
        assert!(matches!(space.copy_from_user(0x40_1ffc, &mut buffer), Err(Errno::EFAULT)), "past the mapped pages");
        let kernel = VirtAddr::from_ptr(&USER_RW).as_u64();
        assert!(matches!(space.copy_from_user(kernel, &mut buffer), Err(Errno::EFAULT)), "kernel memory");
        // the kernel can still write there itself
        space.write_bytes(0x40_1000, b"x").unwrap();
    }

    #[test_case]
    fn test_user_spaces_are_separate() {
        let (mut a, mut b) = (UserSpace::new().unwrap(), UserSpace::new().unwrap());
        let page = VirtAddr::new(0x40_0000);
        a.map(page, 4096, USER_RW).unwrap();
        assert!(b.translate(page).is_none());
        b.map(page, 4096, USER_RW).unwrap();
        assert_ne!(a.translate(page), b.translate(page));
        assert!(matches!(a.unmap(page, 4096), Ok(1)));
        assert!(b.translate(page).is_some());
    }

    /// Nikka's steal-all-frames pattern: with k free frames for every k that is not enough,
    /// creating a space and mapping a range fails without a leak
    #[test_case]
    fn test_out_of_frames_at_every_step() {
        let start = VirtAddr::new((1 << 39) - 2 * 4096);
        let size = 4 * 4096;
        let used = used_frames();
        let mut space = UserSpace::new().unwrap();
        space.map(start, size, USER_RW).unwrap();
        let needed = used_frames() - used;
        drop(space);
        let mut stolen = Vec::with_capacity(crate::memory::total_usable_frames());
        for available in 0..needed {
            with_memory(|m| {
                while let Some(frame) = m.frame_allocator.allocate_frame() {
                    stolen.push(frame);
                }
                for frame in stolen.drain(stolen.len() - available..) {
                    unsafe { m.frame_allocator.deallocate_frame(frame) };
                }
            });
            let guard = crate::testosterone::forbid_frame_leaks();
            match UserSpace::new() {
                None => assert_eq!(available, 0),
                Some(mut space) => assert!(space.map(start, size, USER_RW).is_err(), "{} frames were enough", available),
            }
            drop(guard);
            with_memory(|m| {
                for frame in stolen.drain(..) {
                    unsafe { m.frame_allocator.deallocate_frame(frame) };
                }
            });
        }
    }

    #[test_case]
    fn test_translate_kernel_address() {
        static VALUE: u64 = 42;
        let virt = VirtAddr::from_ptr(&VALUE);
        assert!(virt.as_u64() >= KERNEL_DYNAMIC_START, "kernel is not in the higher half: {:?}", virt);
        let phys = with_memory(|m| m.mapper.translate_addr(virt)).expect("kernel address is not mapped");
        // the same value must be visible through the physical memory mapping
        let through_phys = with_memory(|m| m.mapper.phys_offset() + phys.as_u64());
        assert_eq!(unsafe { *through_phys.as_ptr::<u64>() }, 42);
    }

    #[test_case]
    fn test_map_page() {
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(SCRATCH));
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
        let frame = with_memory(|m| {
            let frame = m.frame_allocator.allocate_frame().expect("out of frames");
            unsafe { m.mapper.map_to(page, frame, flags, &mut m.frame_allocator) }
                .expect("map_to failed")
                .flush();
            frame
        });

        let ptr: *mut u64 = page.start_address().as_mut_ptr();
        unsafe { ptr.write_volatile(0xf00d_cafe) };
        let through_phys = with_memory(|m| m.mapper.phys_offset() + frame.start_address().as_u64());
        assert_eq!(unsafe { through_phys.as_ptr::<u64>().read_volatile() }, 0xf00d_cafe);

        with_memory(|m| {
            let (unmapped, flush) = m.mapper.unmap(page).expect("unmap failed");
            flush.flush();
            assert_eq!(unmapped, frame);
            unsafe { m.frame_allocator.deallocate_frame(frame) };
            free_scratch_tables(m);
        });
        assert!(with_memory(|m| m.mapper.translate_addr(page.start_address())).is_none());
    }

    #[test_case]
    fn test_frames_are_distinct_and_above_low_memory() {
        let (a, b) = with_memory(|m| {
            (m.frame_allocator.allocate_frame().unwrap(), m.frame_allocator.allocate_frame().unwrap())
        });
        assert_ne!(a, b);
        let level_4 = Cr3::read().0;
        let heap = with_memory(|m| m.mapper.translate_addr(VirtAddr::new(HEAP_START))).unwrap();
        for frame in [a, b] {
            assert!(frame.start_address().as_u64() >= LOW_MEMORY_END, "{:?}", frame);
            assert_ne!(frame, level_4, "the active page table frame was handed out");
            assert_ne!(frame.start_address(), heap, "a heap frame was handed out again");
        }
        with_memory(|m| unsafe {
            m.frame_allocator.deallocate_frame(a);
            m.frame_allocator.deallocate_frame(b);
        });
    }

    /// Nikka 2-mm-1 `basic`: the count follows allocations, a freed frame comes back first
    #[test_case]
    fn test_freed_frame_is_reused() {
        let used = used_frames();
        with_memory(|m| {
            let a = m.frame_allocator.allocate_frame().unwrap();
            let b = m.frame_allocator.allocate_frame().unwrap();
            assert_eq!(m.frame_allocator.used_frames(), used + 2);
            unsafe { m.frame_allocator.deallocate_frame(a) };
            assert_eq!(m.frame_allocator.used_frames(), used + 1);
            assert_eq!(m.frame_allocator.allocate_frame(), Some(a), "freed frames are reused LIFO");
            unsafe {
                m.frame_allocator.deallocate_frame(a);
                m.frame_allocator.deallocate_frame(b);
            }
        });
        assert_eq!(used_frames(), used);
    }

    #[test_case]
    fn test_heap_frames_are_known_to_the_pool() {
        // handed out before tracking was enabled, still known to the pool
        let heap = with_memory(|m| m.mapper.translate_addr(VirtAddr::new(HEAP_START))).unwrap();
        assert!(with_memory(|m| m.frame_allocator.is_handed_out(heap.as_u64())));
    }

    #[test_case]
    fn test_interrupt_stack_guards_are_unmapped() {
        let translate = |address: VirtAddr| with_memory(|m| m.mapper.translate_addr(address));
        for stack in crate::gdt::ist_stacks() {
            assert!(translate(stack.guard).is_none(), "guard page of IST {} is mapped", stack.index);
            assert!(translate(stack.guard + 4096u64).is_some(), "IST {} is not mapped", stack.index);
            assert!(translate(stack.top - 1u64).is_some());
        }
    }

    #[test_case]
    fn test_memory_lock_disables_interrupts() {
        assert!(x86_64::instructions::interrupts::are_enabled());
        with_memory(|_| assert!(!x86_64::instructions::interrupts::are_enabled()));
        assert!(x86_64::instructions::interrupts::are_enabled(), "interrupts are back on");
    }

    /// Nikka 2-mm-4 `tlb_flush`: remapping a page shows the new frame right away
    #[test_case]
    fn test_remapped_page_shows_the_new_frame() {
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(SCRATCH + 0x1000));
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
        let marked = |mark: u64| {
            with_memory(|m| {
                let frame = m.frame_allocator.allocate_frame().unwrap();
                let through_phys = m.mapper.phys_offset() + frame.start_address().as_u64();
                unsafe { through_phys.as_mut_ptr::<u64>().write_volatile(mark) };
                frame
            })
        };
        let (old, new) = (marked(1111111), marked(2222222));
        let read = || unsafe { page.start_address().as_ptr::<u64>().read_volatile() };

        with_memory(|m| unsafe { m.mapper.map_to(page, old, flags, &mut m.frame_allocator) }.unwrap().flush());
        assert_eq!(read(), 1111111);
        with_memory(|m| m.mapper.unmap(page).unwrap().1.flush());
        with_memory(|m| unsafe { m.mapper.map_to(page, new, flags, &mut m.frame_allocator) }.unwrap().flush());
        assert_eq!(read(), 2222222, "a stale TLB entry still points to the old frame");
        with_memory(|m| {
            m.mapper.unmap(page).unwrap().1.flush();
            unsafe {
                m.frame_allocator.deallocate_frame(old);
                m.frame_allocator.deallocate_frame(new);
            }
            free_scratch_tables(m);
        });
    }

    /// Nikka 2-mm-4 `garbage_after_unmap`: an unmapped entry is all zero, not just not present
    #[test_case]
    fn test_unmapped_entry_is_cleared() {
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(SCRATCH + 0x2000));
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
        with_memory(|m| {
            let frame = m.frame_allocator.allocate_frame().unwrap();
            unsafe { m.mapper.map_to(page, frame, flags, &mut m.frame_allocator) }.unwrap().flush();
            m.mapper.unmap(page).unwrap().1.flush();
            // walk down to the level 1 table through the physical memory mapping
            let offset = m.mapper.phys_offset();
            let mut table: &PageTable = m.mapper.level_4_table();
            for index in [page.p4_index(), page.p3_index(), page.p2_index()] {
                let next = table[index].addr();
                table = unsafe { &*(offset + next.as_u64()).as_ptr::<PageTable>() };
            }
            assert_eq!(table[page.p1_index()].addr().as_u64(), 0);
            assert!(table[page.p1_index()].is_unused(), "{:?}", table[page.p1_index()]);
            unsafe { m.frame_allocator.deallocate_frame(frame) };
            free_scratch_tables(m);
        });
    }

    /// Nikka 2-mm-4 `huge_page`: the physical memory mapping uses 2 MiB pages, translation
    /// through them gives the exact address
    #[test_case]
    fn test_translation_through_huge_pages() {
        use x86_64::structures::paging::mapper::{MappedFrame, TranslateResult};
        let phys = 0x20_1234u64;
        let virt = with_memory(|m| m.mapper.phys_offset()) + phys;
        match with_memory(|m| m.mapper.translate(virt)) {
            TranslateResult::Mapped { frame, offset, .. } => {
                assert!(matches!(frame, MappedFrame::Size2MiB(_)), "{:?}", frame);
                assert_eq!(frame.start_address().as_u64() + offset, phys);
            }
            other => panic!("{:?}", other),
        }
    }

    #[test_case]
    fn test_lower_half_is_empty() {
        let used = with_memory(|m| m.mapper.level_4_table().iter().take(256).filter(|e| !e.is_unused()).count());
        assert_eq!(used, 0, "lower half level 4 entries in use");
        assert!(unsafe { LOWER_HALF_ENTRIES_CLEARED } > 0, "the bootloader left nothing to clear?");
    }

    #[test_case]
    fn test_heap_has_unmapped_neighbours() {
        let translate = |address: u64| with_memory(|m| m.mapper.translate_addr(VirtAddr::new(address)));
        assert!(translate(HEAP_START).is_some());
        assert!(translate(HEAP_START + HEAP_SIZE - 1).is_some());
        assert!(translate(HEAP_START - 1).is_none(), "page before the heap is mapped");
        assert!(translate(HEAP_START + HEAP_SIZE).is_none(), "page after the heap is mapped");
    }

    #[test_case]
    fn test_physical_memory_mapping_covers_low_4_gib() {
        // the bootloader maps at least 4 GiB, so device memory (APIC, PCI) is reachable
        let phys_offset = with_memory(|m| m.mapper.phys_offset());
        for phys in [0u64, 0xfee0_0000, 0xffff_f000] {
            let virt = phys_offset + phys;
            assert!(with_memory(|m| m.mapper.translate_addr(virt)).is_some(), "{:#x}", phys);
        }
    }

    #[test_case]
    fn test_with_memory_releases_the_lock() {
        with_memory(|_| {});
        with_memory(|_| {});
    }
}
