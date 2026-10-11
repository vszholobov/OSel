/// Global Descriptor Table module

use core::ptr::addr_of;
use lazy_static::lazy_static;
use x86_64::VirtAddr;
use x86_64::structures::gdt::SegmentSelector;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable};
use x86_64::structures::tss::TaskStateSegment;

pub struct Selectors {
    pub code_selector: SegmentSelector,
    pub data_selector: SegmentSelector,
    pub tss_selector: SegmentSelector,
}

pub fn init() {
    use x86_64::instructions::segmentation::{CS, DS, ES, FS, GS, SS, Segment};
    use x86_64::instructions::tables::load_tss;

    GDT.0.load();
    unsafe {
        CS::set_reg(GDT.1.code_selector);
        // the firmware and the bootloader leave their own selectors in the data
        // segment registers; they may point past our GDT and fault once reloaded,
        // e.g. on `iretq` (SS) or a future context switch (FS, GS)
        SS::set_reg(GDT.1.data_selector);
        DS::set_reg(GDT.1.data_selector);
        ES::set_reg(GDT.1.data_selector);
        FS::set_reg(SegmentSelector(0));
        GS::set_reg(SegmentSelector(0));
        // the LDT register may still hold its reset value (a table at address 0),
        // a null LDT makes every LDT selector fault cleanly with #GP
        core::arch::asm!("lldt {0:x}", in(reg) 0u16, options(nostack, preserves_flags));
        load_tss(GDT.1.tss_selector);
    }
}

/// Selectors of the kernel GDT
pub fn selectors() -> &'static Selectors {
    &GDT.1
}

lazy_static! {
    static ref GDT: (GlobalDescriptorTable, Selectors) = {
        let mut gdt = GlobalDescriptorTable::new();
        let code_selector = gdt.append(Descriptor::kernel_code_segment());
        let data_selector = gdt.append(Descriptor::kernel_data_segment());
        let tss_selector = gdt.append(Descriptor::tss_segment(&TSS));
        (
            gdt,
            Selectors {
                code_selector,
                data_selector,
                tss_selector,
            },
        )
    };
}

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;
pub const NMI_IST_INDEX: u16 = 1;
pub const MACHINE_CHECK_IST_INDEX: u16 = 2;

const PAGE_SIZE: usize = 4096;
/// Each handler on these stacks may draw the whole panic screen
pub const IST_STACK_SIZE: usize = 8 * PAGE_SIZE;

/// The first page is unmapped by `memory::init`: overflowing the stack faults
/// instead of silently overwriting the statics below it
#[repr(C, align(4096))]
struct GuardedStack([u8; PAGE_SIZE + IST_STACK_SIZE]);

static mut DOUBLE_FAULT_STACK: GuardedStack = GuardedStack([0; PAGE_SIZE + IST_STACK_SIZE]);
static mut NMI_STACK: GuardedStack = GuardedStack([0; PAGE_SIZE + IST_STACK_SIZE]);
static mut MACHINE_CHECK_STACK: GuardedStack = GuardedStack([0; PAGE_SIZE + IST_STACK_SIZE]);

/// An interrupt stack table entry: its guard page and the top the stack grows down from
#[derive(Debug, Clone, Copy)]
pub struct IstStack {
    pub index: u16,
    pub guard: VirtAddr,
    pub top: VirtAddr,
}

/// Every interrupt stack of the TSS
pub fn ist_stacks() -> [IstStack; 3] {
    let stack = |index, start: *const GuardedStack| {
        let guard = VirtAddr::from_ptr(start);
        IstStack { index, guard, top: guard + (PAGE_SIZE + IST_STACK_SIZE) as u64 }
    };
    [
        stack(DOUBLE_FAULT_IST_INDEX, addr_of!(DOUBLE_FAULT_STACK)),
        stack(NMI_IST_INDEX, addr_of!(NMI_STACK)),
        stack(MACHINE_CHECK_IST_INDEX, addr_of!(MACHINE_CHECK_STACK)),
    ]
}

lazy_static! {
    static ref TSS: TaskStateSegment = {
        let mut tss = TaskStateSegment::new();
        for stack in ist_stacks() {
            tss.interrupt_stack_table[stack.index as usize] = stack.top;
        }
        tss
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use x86_64::instructions::segmentation::{CS, DS, ES, FS, GS, SS, Segment};

    #[test_case]
    fn test_segment_registers() {
        let selectors = selectors();
        assert_eq!(CS::get_reg(), selectors.code_selector);
        assert_eq!(SS::get_reg(), selectors.data_selector);
        assert_eq!(DS::get_reg(), selectors.data_selector);
        assert_eq!(ES::get_reg(), selectors.data_selector);
        assert_eq!(FS::get_reg(), SegmentSelector(0));
        assert_eq!(GS::get_reg(), SegmentSelector(0));
    }

    #[test_case]
    fn test_task_and_ldt_registers() {
        let (task, ldt): (u16, u16);
        unsafe { core::arch::asm!("str {0:x}", "sldt {1:x}", out(reg) task, out(reg) ldt, options(nostack)) };
        assert_eq!(task, selectors().tss_selector.0);
        assert_eq!(ldt, 0);
    }

    #[test_case]
    fn test_interrupt_stacks_are_in_the_tss() {
        // the TSS is packed, copy the field out before comparing
        let table = { TSS.interrupt_stack_table };
        let stacks = ist_stacks();
        for stack in stacks {
            assert_eq!(table[stack.index as usize], stack.top);
            assert_eq!(stack.guard.as_u64() % PAGE_SIZE as u64, 0);
        }
        // separate stacks: a nested NMI during a double fault must not share one
        assert!(stacks[0].top <= stacks[1].guard || stacks[1].top <= stacks[0].guard);
        assert!(stacks[1].top <= stacks[2].guard || stacks[2].top <= stacks[1].guard);
    }
}
