//! user module runs programs in ring 3
//!
//! `run` enters a program with `iretq` and returns when the program traps. Exceptions
//! from ring 3 go through the entry stubs of `interrupts`: they push every register into
//! a `TrapFrame` on the stack, `user_trap` copies it into the caller's frame and returns
//! from `enter_user` on the kernel stack it left. One CPU and one program at a time: the
//! saved kernel stack pointer is a plain static, no `swapgs` is needed.

use crate::gdt;
use crate::memory::{self, USER_END, UserSpace};
use core::arch::naked_asm;
use osel_core::syscall::SYSCALL_VECTOR;
use osel_core::trap::{GENERAL_PROTECTION, TRAP_FRAME_WORDS, TrapFrame};

/// Kernel stack pointer of the `enter_user` that runs the current program
pub(crate) static mut KERNEL_RSP: u64 = 0;
/// The address space of the running program, for system calls
static mut RUNNING: *mut UserSpace = core::ptr::null_mut();
/// CR2 when the program trapped: the faulting address of a page fault
static mut TRAP_CR2: u64 = 0;

/// How a program's run ended
#[derive(Debug, Clone, Copy)]
pub enum Stop {
    /// `exit` or `exit_group` with this status
    Exit(u8),
    /// An exception the program caused
    Fault(Trap),
}

impl Stop {
    /// The exception, panics if the program exited
    #[track_caller]
    pub fn fault(self) -> Trap {
        match self {
            Stop::Fault(trap) => trap,
            Stop::Exit(code) => panic!("the program exited with {} instead of a fault", code),
        }
    }

    /// The exit status, panics if the program faulted
    #[track_caller]
    pub fn exit_code(self) -> u8 {
        match self {
            Stop::Exit(code) => code,
            Stop::Fault(trap) => panic!("the program faulted with vector {} instead of exiting", trap.vector),
        }
    }
}

/// An exception that ended a program
#[derive(Debug, Clone, Copy)]
pub struct Trap {
    pub vector: u64,
    pub error_code: u64,
    /// The faulting address for a page fault
    pub cr2: u64,
}

/// A fresh frame for a program starting at `entry` with stack `rsp`
pub fn initial_frame(entry: u64, rsp: u64) -> TrapFrame {
    let selectors = gdt::selectors();
    TrapFrame::new_user(entry, rsp, selectors.user_code_selector.0, selectors.user_data_selector.0)
}

/// Runs the program in `space` from `frame` until it exits or traps. The frame then holds
/// every register of the program at that moment. The kernel's own page table is active again
/// afterwards. Interrupts must be on: a held `IrqSpinlock` would deadlock the timer.
/// The frame is forced to ring 3 first; a `rip` outside the lower half is a #GP of the
/// program right away, without entering it.
pub fn run(space: &mut UserSpace, frame: &mut TrapFrame) -> Stop {
    assert!(x86_64::instructions::interrupts::are_enabled(), "entering a program with interrupts off or a lock held");
    // one saved kernel stack and one running space: a run inside a system call would lose them
    assert!(unsafe { RUNNING.is_null() }, "a program is already running");
    let selectors = gdt::selectors();
    if !frame.make_user(selectors.user_code_selector.0, selectors.user_data_selector.0, USER_END) {
        frame.vector = GENERAL_PROTECTION;
        frame.error_code = 0;
        return Stop::Fault(Trap { vector: GENERAL_PROTECTION, error_code: 0, cr2: 0 });
    }
    space.activate();
    unsafe {
        RUNNING = space;
        enter_user(frame, gdt::kernel_stack_slot());
        RUNNING = core::ptr::null_mut();
    }
    memory::activate_kernel_space();
    gdt::reload_data_segments();
    if frame.vector == SYSCALL_VECTOR {
        // `exit` keeps its argument where the program put it
        return Stop::Exit(frame.rdi as u8);
    }
    let cr2 = if frame.vector == osel_core::trap::PAGE_FAULT { unsafe { TRAP_CR2 } } else { 0 };
    Stop::Fault(Trap { vector: frame.vector, error_code: frame.error_code, cr2 })
}

/// The address space of the program whose system call is running
///
/// # Safety
/// Only from a system call: `run` is on the stack below and holds the space, but does not
/// touch it until the program leaves; one CPU, so nothing else does either.
pub(crate) unsafe fn running_space() -> &'static mut UserSpace {
    unsafe { RUNNING.as_mut() }.expect("no program is running")
}

/// Saves the callee-saved registers and the flags, makes the current stack the one the
/// CPU switches to from ring 3, then loads `frame` and `iretq`s into the program.
/// Returns when `user_trap` runs.
#[unsafe(naked)]
unsafe extern "C" fn enter_user(frame: *mut TrapFrame, kernel_stack_slot: *mut u64) {
    naked_asm!(
        "push rbx",
        "push rbp",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "pushfq",
        "push rdi",
        // nothing may interrupt between switching to the frame and `iretq`
        "cli",
        "mov [rip + {kernel_rsp}], rsp",
        // RSP0 of the TSS: traps from ring 3 land below what was just pushed (the CPU
        // aligns the frame to 16 bytes, so maybe with a gap of 8)
        "mov [rsi], rsp",
        "mov rsp, rdi",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rbp",
        "pop rdi",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        "pop rbx",
        "pop rax",
        // vector and error code
        "add rsp, 16",
        "iretq",
        kernel_rsp = sym KERNEL_RSP,
    )
}

/// Reached by a `jmp` from an entry stub after a trap from ring 3, with the vector and the
/// error code pushed on top of what the CPU pushed. Completes the `TrapFrame` on the
/// stack and leaves the run.
#[unsafe(naked)]
pub(crate) unsafe extern "C" fn user_trap() {
    naked_asm!(
        "push rax",
        "push rbx",
        "push rcx",
        "push rdx",
        "push rsi",
        "push rdi",
        "push rbp",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "jmp {leave_user}",
        leave_user = sym leave_user,
    )
}

/// Reached by a `jmp` with a complete `TrapFrame` at the stack pointer and interrupts off:
/// copies it to the frame `enter_user` got and returns from `enter_user`
#[unsafe(naked)]
pub(crate) unsafe extern "C" fn leave_user() {
    naked_asm!(
        // the program may have left DF set, and `rep movsq` follows
        "cld",
        "mov rax, cr2",
        "mov [rip + {cr2}], rax",
        "mov rsi, rsp",
        "mov rax, [rip + {kernel_rsp}]",
        // the frame pointer `enter_user` pushed last
        "mov rdi, [rax]",
        "mov ecx, {words}",
        "rep movsq",
        "lea rsp, [rax + 8]",
        // the kernel's flags, interrupts on again
        "popfq",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbp",
        "pop rbx",
        "ret",
        cr2 = sym TRAP_CR2,
        kernel_rsp = sym KERNEL_RSP,
        words = const TRAP_FRAME_WORDS,
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::program::{self, Loaded};
    use alloc::vec::Vec;
    use core::arch::global_asm;
    use osel_core::elf::{ElfBuilder, PF_R, PF_W, PF_X};
    use osel_core::trap::RFLAGS_IF;
    use x86_64::VirtAddr;

    pub(crate) const TEXT: u64 = 0x40_0000;
    const DATA: u64 = 0x60_0000;

    // Test programs: machine code between labels, copied into an ELF at `TEXT`. Only
    // position independent code, the bytes run far from where they were assembled.
    global_asm!(
        ".pushsection .rodata.osel_user_programs, \"a\"",
        // every register gets a pattern, then a write to address 0
        "osel_patterns:",
        "mov rax, 0x0101010101010101",
        "mov rbx, 0x0202020202020202",
        "mov rcx, 0x0303030303030303",
        "mov rdx, 0x0404040404040404",
        "mov rsi, 0x0505050505050505",
        "mov rdi, 0x0606060606060606",
        "mov rbp, 0x0707070707070707",
        "mov r8, 0x0808080808080808",
        "mov r9, 0x0909090909090909",
        "mov r10, 0x0a0a0a0a0a0a0a0a",
        "mov r11, 0x0b0b0b0b0b0b0b0b",
        "mov r12, 0x0c0c0c0c0c0c0c0c",
        "mov r13, 0x0d0d0d0d0d0d0d0d",
        "mov r14, 0x0e0e0e0e0e0e0e0e",
        "mov r15, 0x0f0f0f0f0f0f0f0f",
        "osel_patterns_fault:",
        "mov qword ptr [0], rax",
        "osel_patterns_end:",
        // the start state: argc from the stack into rax, then fault
        "osel_start_state:",
        "mov rax, [rsp]",
        "mov qword ptr [0], rax",
        "osel_start_state_end:",
        // Nikka `user_mode_page_fault`: no stack at all
        "osel_no_stack:",
        "xor esp, esp",
        "push rax",
        "osel_no_stack_end:",
        // spins long enough for timer ticks even on fast hardware, with DF set, then faults
        "osel_spin:",
        "std",
        "mov rcx, 200000000",
        "2:",
        "dec rcx",
        "jnz 2b",
        "mov qword ptr [0], rax",
        "osel_spin_end:",
        "osel_hlt:", "hlt", "osel_hlt_end:",
        "osel_cli:", "cli", "osel_cli_end:",
        "osel_in:", "in al, 0x60", "osel_in_end:",
        "osel_int80:", "int 0x80", "osel_int80_end:",
        "osel_int3:", "int3", "osel_int3_end:",
        "osel_ud2:", "ud2", "osel_ud2_end:",
        "osel_wrmsr:", "xor eax, eax", "xor edx, edx", "mov ecx, 0xc0000080", "wrmsr", "osel_wrmsr_end:",
        "osel_rdmsr:", "mov ecx, 0xc0000080", "rdmsr", "osel_rdmsr_end:",
        "osel_out:", "out 0x80, al", "osel_out_end:",
        // calls itself until the stack runs into the guard page below it
        "osel_recursion:", "2:", "call 2b", "osel_recursion_end:",
        "osel_divide:", "xor ecx, ecx", "div rcx", "osel_divide_end:",
        "osel_kernel_jump:", "mov rax, 0xffff800000000000", "jmp rax", "osel_kernel_jump_end:",
        "osel_kernel_read:", "mov rax, 0xffff800000000000", "mov rax, [rax]", "osel_kernel_read_end:",
        "osel_write_text:", "lea rax, [rip]", "mov byte ptr [rax], 0", "osel_write_text_end:",
        "osel_run_data:", "mov rax, 0x600000", "jmp rax", "osel_run_data_end:",
        "osel_bad_stack:", "mov rsp, 0x800000000008", "push rax", "osel_bad_stack_end:",
        "osel_trap_flag:", "pushfq", "or qword ptr [rsp], 0x100", "popfq", "nop", "nop", "osel_trap_flag_end:",
        ".popsection",
    );

    /// The machine code between two labels of a `global_asm!` test program
    macro_rules! code {
        ($start:ident, $end:ident) => {{
            unsafe extern "C" {
                static $start: u8;
                static $end: u8;
            }
            let start = core::ptr::addr_of!($start);
            let len = core::ptr::addr_of!($end) as usize - start as usize;
            unsafe { core::slice::from_raw_parts(start, len) }
        }};
    }
    pub(crate) use code;

    /// `code` as a program: text at `TEXT`, a data page at `DATA`
    pub(crate) fn load(code: &[u8]) -> Loaded {
        let file = ElfBuilder::new(TEXT).segment(TEXT, PF_R | PF_X, code, code.len() as u64).segment(DATA, PF_R | PF_W, &[0xc3], 0x1000).build();
        program::load_file(&file[..], &[b"test"], &[]).expect("the test program does not load")
    }

    fn run_code(code: &[u8]) -> (TrapFrame, Trap, Loaded) {
        let (frame, stop, loaded) = run_program(code);
        (frame, stop.fault(), loaded)
    }

    pub(crate) fn run_program(code: &[u8]) -> (TrapFrame, Stop, Loaded) {
        let mut loaded = load(code);
        let mut frame = initial_frame(loaded.entry, loaded.rsp);
        let stop = run(&mut loaded.space, &mut frame);
        (frame, stop, loaded)
    }

    pub(crate) fn kernel_is_fine() {
        assert!(x86_64::instructions::interrupts::are_enabled(), "interrupts are back on");
        let flags = x86_64::registers::rflags::read();
        assert!(!flags.contains(x86_64::registers::rflags::RFlags::DIRECTION_FLAG), "the program's DF leaked");
        let selectors = gdt::selectors();
        use x86_64::instructions::segmentation::{DS, ES, SS, Segment};
        assert_eq!(SS::get_reg(), selectors.data_selector);
        assert_eq!(DS::get_reg(), selectors.data_selector);
        assert_eq!(ES::get_reg(), selectors.data_selector);
        let values: Vec<u64> = (0..100).collect();
        assert_eq!(values.iter().sum::<u64>(), 4950);
    }

    /// Nikka `user_context_saved`: the frame holds exactly what the program had
    #[test_case]
    fn test_a_fault_saves_every_register() {
        let code = code!(osel_patterns, osel_patterns_end);
        let fault_offset = (code!(osel_patterns, osel_patterns_fault)).len() as u64;
        let (frame, trap, loaded) = run_code(code);
        kernel_is_fine();
        assert_eq!(trap.vector, 14);
        assert_eq!(trap.cr2, 0);
        assert_eq!(trap.error_code, 0b110, "not present, write, user");
        assert_eq!(frame.rip, TEXT + fault_offset);
        assert_eq!(frame.rsp, loaded.rsp);
        assert_eq!(frame.cs, gdt::selectors().user_code_selector.0 as u64);
        assert_eq!(frame.ss, gdt::selectors().user_data_selector.0 as u64);
        assert!(frame.rflags & RFLAGS_IF != 0);
        let registers = [
            frame.rax, frame.rbx, frame.rcx, frame.rdx, frame.rsi, frame.rdi, frame.rbp, frame.r8, frame.r9, frame.r10,
            frame.r11, frame.r12, frame.r13, frame.r14, frame.r15,
        ];
        for (index, value) in registers.into_iter().enumerate() {
            assert_eq!(value, (index as u64 + 1) * 0x0101_0101_0101_0101, "register {}", index);
        }
    }

    #[test_case]
    fn test_a_program_starts_with_its_stack_and_zero_registers() {
        let (frame, trap, _loaded) = run_code(code!(osel_start_state, osel_start_state_end));
        kernel_is_fine();
        assert_eq!(trap.vector, 14);
        assert_eq!(frame.rax, 1, "argc");
        let others = [
            frame.rbx, frame.rcx, frame.rdx, frame.rsi, frame.rdi, frame.rbp, frame.r8, frame.r9, frame.r10, frame.r11,
            frame.r12, frame.r13, frame.r14, frame.r15,
        ];
        assert_eq!(others, [0; 14], "nothing of the kernel leaks into a new program");
        // the CPU adds RF to the flags it saves for a fault
        assert_eq!(frame.rflags & !(1 << 16), 0x202);
    }

    /// Nikka `user_mode_page_fault`: a program without a stack can't hurt the kernel,
    /// the CPU switches to the kernel stack from the TSS
    #[test_case]
    fn test_a_program_without_a_stack() {
        let (frame, trap, _loaded) = run_code(code!(osel_no_stack, osel_no_stack_end));
        kernel_is_fine();
        assert_eq!(trap.vector, 14);
        assert_eq!(frame.rsp, 0);
        assert_eq!(trap.cr2, 0xffff_ffff_ffff_fff8);
    }

    #[test_case]
    fn test_the_timer_runs_while_a_program_spins() {
        let mut loaded = load(code!(osel_spin, osel_spin_end));
        let mut frame = initial_frame(loaded.entry, loaded.rsp);
        let before = crate::interrupts::ticks();
        let trap = run(&mut loaded.space, &mut frame).fault();
        kernel_is_fine();
        assert_eq!(trap.vector, 14);
        assert!(crate::interrupts::ticks() > before, "no timer interrupt while the program ran");
        assert!(frame.rflags & (1 << 10) != 0, "the program's DF is in its frame");
    }

    /// Everything a program must not do ends the program with the right exception
    #[test_case]
    fn test_forbidden_instructions_end_the_program() {
        // a non-canonical stack address is #SS on hardware, QEMU raises #GP
        let cases: [(&str, &[u8], &[u64], Option<u64>); 16] = [
            ("hlt", code!(osel_hlt, osel_hlt_end), &[13], Some(0)),
            ("cli", code!(osel_cli, osel_cli_end), &[13], Some(0)),
            ("in", code!(osel_in, osel_in_end), &[13], Some(0)),
            ("int 0x80", code!(osel_int80, osel_int80_end), &[13], Some(0x80 * 8 + 2)),
            ("int3", code!(osel_int3, osel_int3_end), &[13], Some(3 * 8 + 2)),
            ("ud2", code!(osel_ud2, osel_ud2_end), &[6], None),
            ("wrmsr", code!(osel_wrmsr, osel_wrmsr_end), &[13], Some(0)),
            ("rdmsr", code!(osel_rdmsr, osel_rdmsr_end), &[13], Some(0)),
            ("out", code!(osel_out, osel_out_end), &[13], Some(0)),
            ("divide by zero", code!(osel_divide, osel_divide_end), &[0], None),
            ("jump into the kernel", code!(osel_kernel_jump, osel_kernel_jump_end), &[14], None),
            ("read kernel memory", code!(osel_kernel_read, osel_kernel_read_end), &[14], None),
            ("write its own code", code!(osel_write_text, osel_write_text_end), &[14], Some(0b111)),
            ("run its data", code!(osel_run_data, osel_run_data_end), &[14], Some(0b10101)),
            ("non-canonical stack", code!(osel_bad_stack, osel_bad_stack_end), &[12, 13], Some(0)),
            ("trap flag", code!(osel_trap_flag, osel_trap_flag_end), &[1], None),
        ];
        for (name, code, vector, error_code) in cases {
            let (_frame, trap, _loaded) = run_code(code);
            assert!(vector.contains(&trap.vector), "{}: vector {}", name, trap.vector);
            if let Some(error_code) = error_code {
                assert_eq!(trap.error_code, error_code, "{}", name);
            }
            kernel_is_fine();
        }
    }

    #[test_case]
    fn test_a_stack_overflow_ends_the_program() {
        let (_frame, trap, _loaded) = run_code(code!(osel_recursion, osel_recursion_end));
        kernel_is_fine();
        assert_eq!(trap.vector, 14);
        assert_eq!(trap.cr2, crate::program::STACK_BOTTOM - 8, "the push into the guard page");
    }

    /// The design's leak check: the same program a hundred times, nothing is left behind
    #[test_case]
    fn test_many_runs_leak_nothing() {
        let frames = memory::used_frames();
        for _ in 0..100 {
            let (_frame, trap, _loaded) = run_code(code!(osel_patterns, osel_patterns_end));
            assert_eq!(trap.vector, 14);
        }
        assert_eq!(memory::used_frames(), frames);
    }

    /// A frame built by the kernel from program data can't take the program to ring 0
    #[test_case]
    fn test_a_frame_is_forced_to_ring_3() {
        let mut loaded = load(code!(osel_ud2, osel_ud2_end));
        let mut frame = initial_frame(loaded.entry, loaded.rsp);
        frame.cs = gdt::selectors().code_selector.0 as u64;
        frame.rflags |= 3 << 12;
        let trap = run(&mut loaded.space, &mut frame).fault();
        assert_eq!(trap.vector, 6, "ran in ring 3 and hit ud2");
        assert_eq!(frame.cs, gdt::selectors().user_code_selector.0 as u64);
        let mut kernel_rip = initial_frame(0xffff_8000_0000_0000, loaded.rsp);
        assert_eq!(run(&mut loaded.space, &mut kernel_rip).fault().vector, 13, "a kernel address is not entered");
        kernel_is_fine();
    }

    #[test_case]
    fn test_the_kernel_table_is_active_after_a_program() {
        let (_frame, _trap, mut loaded) = run_code(code!(osel_ud2, osel_ud2_end));
        assert!(!loaded.space.is_active());
        assert!(loaded.space.translate(VirtAddr::new(TEXT)).is_some());
    }
}
