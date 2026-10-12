//! The state of a program when it enters the kernel
//!
//! The entry code pushes the general purpose registers right below what the CPU pushed,
//! so `TrapFrame` is laid out as the stack looks then. The assembly depends on the field
//! offsets: they are checked by the tests below.

use crate::exception::{Details, ExceptionReport, Frame};

/// RFLAGS: interrupts enabled
pub const RFLAGS_IF: u64 = 1 << 9;
/// RFLAGS: bit 1 is always set
pub const RFLAGS_RESERVED: u64 = 1 << 1;

/// RFLAGS bits a program may set itself: the arithmetic flags, TF, DF, AC and ID. IOPL,
/// NT, RF and VM stay clear, IF stays set.
pub const RFLAGS_USER: u64 = 0b1 | 0b100 | 0b1_0000 | 0b100_0000 | 0b1000_0000 | (1 << 8) | (1 << 10) | (1 << 11) | (1 << 18) | (1 << 21);

pub const PAGE_FAULT: u64 = 14;
pub const GENERAL_PROTECTION: u64 = 13;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrapFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    /// Interrupt vector
    pub vector: u64,
    /// The CPU's error code, 0 for vectors without one
    pub error_code: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

/// Number of 64-bit words in a `TrapFrame`
pub const TRAP_FRAME_WORDS: usize = 22;

impl TrapFrame {
    /// A program about to start at `entry` with stack `rsp`: every register is 0 (the
    /// System V ABI wants RDX = 0, nothing else may leak from the kernel), interrupts on
    pub fn new_user(entry: u64, rsp: u64, code_selector: u16, data_selector: u16) -> TrapFrame {
        TrapFrame {
            rip: entry,
            rsp,
            cs: code_selector as u64,
            ss: data_selector as u64,
            rflags: RFLAGS_IF | RFLAGS_RESERVED,
            ..TrapFrame::default()
        }
    }

    /// Makes the frame safe to `iretq` into ring 3, whoever filled it: the program's code
    /// and stack selectors, only the flags a program may have, interrupts on. `false` if
    /// `rip` is outside the lower half: `iretq` there would fault in the kernel.
    pub fn make_user(&mut self, code_selector: u16, data_selector: u16, user_end: u64) -> bool {
        self.cs = code_selector as u64;
        self.ss = data_selector as u64;
        self.rflags = self.rflags & RFLAGS_USER | RFLAGS_IF | RFLAGS_RESERVED;
        self.rip < user_end
    }

    /// The report the kernel prints when the trap ends a program; `cr2` is the faulting
    /// address of a page fault
    pub fn report(&self, cr2: u64) -> ExceptionReport<'static> {
        let frame = Frame { rip: self.rip, rsp: self.rsp, cs: self.cs as u16, ss: self.ss as u16, rflags: self.rflags };
        let details = match self.vector {
            PAGE_FAULT => Details::PageFault { code: self.error_code, address: cr2 },
            10..=13 => Details::Selector(self.error_code),
            8 | 17 | 21 | 29 | 30 => Details::ErrorCode(self.error_code),
            _ => Details::None,
        };
        ExceptionReport { name: exception_name(self.vector), frame, details }
    }
}

/// Name of a CPU exception as the reports show it
pub fn exception_name(vector: u64) -> &'static str {
    match vector {
        0 => "DIVIDE ERROR",
        1 => "DEBUG",
        2 => "NON-MASKABLE INTERRUPT",
        3 => "BREAKPOINT",
        4 => "OVERFLOW",
        5 => "BOUND RANGE EXCEEDED",
        6 => "INVALID OPCODE",
        7 => "DEVICE NOT AVAILABLE",
        8 => "DOUBLE FAULT",
        10 => "INVALID TSS",
        11 => "SEGMENT NOT PRESENT",
        12 => "STACK SEGMENT FAULT",
        13 => "GENERAL PROTECTION FAULT",
        14 => "PAGE FAULT",
        16 => "X87 FLOATING POINT",
        17 => "ALIGNMENT CHECK",
        18 => "MACHINE CHECK",
        19 => "SIMD FLOATING POINT",
        20 => "VIRTUALIZATION",
        21 => "CONTROL PROTECTION",
        28 => "HYPERVISOR INJECTION EXCEPTION",
        29 => "VMM COMMUNICATION EXCEPTION",
        30 => "SECURITY EXCEPTION",
        _ => "UNKNOWN",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{offset_of, size_of};

    /// The entry and exit assembly in `src/user.rs` relies on these offsets
    #[test]
    fn layout_matches_the_stack() {
        assert_eq!(size_of::<TrapFrame>(), TRAP_FRAME_WORDS * 8);
        assert_eq!(offset_of!(TrapFrame, r15), 0);
        assert_eq!(offset_of!(TrapFrame, rax), 14 * 8);
        assert_eq!(offset_of!(TrapFrame, vector), 15 * 8);
        assert_eq!(offset_of!(TrapFrame, error_code), 16 * 8);
        // what the CPU pushes, in its order from the lowest address
        assert_eq!(offset_of!(TrapFrame, rip), 17 * 8);
        assert_eq!(offset_of!(TrapFrame, cs), 18 * 8);
        assert_eq!(offset_of!(TrapFrame, rflags), 19 * 8);
        assert_eq!(offset_of!(TrapFrame, rsp), 20 * 8);
        assert_eq!(offset_of!(TrapFrame, ss), 21 * 8);
    }

    #[test]
    fn a_new_program_starts_clean() {
        let frame = TrapFrame::new_user(0x40_1000, 0x7fff_ffff_e000, 0x23, 0x1b);
        assert_eq!((frame.rip, frame.rsp, frame.cs, frame.ss), (0x40_1000, 0x7fff_ffff_e000, 0x23, 0x1b));
        assert_eq!(frame.rflags, 0x202);
        assert_eq!(frame.rdx, 0);
        assert_eq!(TrapFrame { rip: 0, rsp: 0, cs: 0, ss: 0, rflags: 0, ..frame }, TrapFrame::default());
    }

    #[test]
    fn a_frame_from_anywhere_is_made_safe() {
        let mut frame = TrapFrame { cs: 0x08, ss: 0x10, rflags: u64::MAX, rip: 0x40_1000, ..TrapFrame::default() };
        assert!(frame.make_user(0x23, 0x1b, 1 << 47));
        assert_eq!((frame.cs, frame.ss), (0x23, 0x1b), "never back to ring 0");
        assert_eq!(frame.rflags & (3 << 12), 0, "no IOPL");
        assert_eq!(frame.rflags & ((1 << 14) | (1 << 16) | (1 << 17)), 0, "no NT, RF, VM");
        assert!(frame.rflags & RFLAGS_IF != 0);
        assert!(frame.rflags & (1 << 10) != 0, "DF is the program's business");
        frame.rip = 0xffff_8000_0000_0000;
        assert!(!frame.make_user(0x23, 0x1b, 1 << 47), "a kernel address");
        frame.rip = 1 << 47;
        assert!(!frame.make_user(0x23, 0x1b, 1 << 47), "non-canonical");
    }

    #[test]
    fn reports_name_the_exception() {
        let frame = TrapFrame { vector: 14, error_code: 0b110, rip: 0x40_1000, ..TrapFrame::default() };
        let text = std::format!("{}", frame.report(0));
        assert!(text.contains("EXCEPTION: PAGE FAULT"), "{text}");
        assert!(text.contains("by user"), "{text}");
        let gp = TrapFrame { vector: 13, error_code: 0x80 * 8 + 2, ..TrapFrame::default() };
        let text = std::format!("{}", gp.report(0));
        assert!(text.contains("GENERAL PROTECTION FAULT") && text.contains("IDT entry 128"), "{text}");
        assert_eq!(exception_name(9), "UNKNOWN");
    }
}
