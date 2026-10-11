//! memory module manages page tables and physical frames
//!
//! The address space layout and the frame allocator live in `osel_core::memory`.

use bootloader_api::info::MemoryRegion;
use crate::sync::IrqSpinlock;
use x86_64::VirtAddr;
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::{Mapper, OffsetPageTable, Page, PageTable, Size4KiB};

pub use osel_core::memory::*;

pub struct Memory {
    pub mapper: OffsetPageTable<'static>,
    pub frame_allocator: RegionFrameAllocator<'static>,
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
    let level_4_table = unsafe { active_level_4_table(physical_memory_offset) };
    let cleared = clear_lower_half(level_4_table);
    unsafe {
        LOWER_HALF_ENTRIES_CLEARED = cleared;
        MEMORY_REGIONS = memory_regions;
    }
    let mut mapper = unsafe { OffsetPageTable::new(level_4_table, physical_memory_offset) };
    unmap_interrupt_stack_guards(&mut mapper);
    let frame_allocator = unsafe { RegionFrameAllocator::new(memory_regions) };
    *MEMORY.lock() = Some(Memory { mapper, frame_allocator });
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
    RegionFrameAllocator::count_frames(unsafe { MEMORY_REGIONS })
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
    use x86_64::structures::paging::{FrameAllocator, Mapper, Page, PageTableFlags, Size4KiB, Translate};

    /// A free spot after the heap for test mappings
    const SCRATCH: u64 = HEAP_START + HEAP_SIZE + 0x10_0000;

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

        with_memory(|m| m.mapper.unmap(page).expect("unmap failed").1.flush());
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
