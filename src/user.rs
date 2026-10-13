//! user module runs programs in ring 3
//!
//! A program is a process with one thread (`scheduler`). Exceptions from ring 3 go through
//! the entry stubs of `interrupts`: they push every register into a `TrapFrame` at the top
//! of the thread's kernel stack, and `user_trap` hands it to `user_fault`: that maps the
//! page of a first touch or ends the thread. `run` starts a
//! program from a frame and waits for it to stop.

use crate::gdt;
use crate::memory::{USER_END, UserSpace};
use crate::scheduler::{self, Process};
use crate::tty::Tty;
use alloc::sync::Arc;
use core::arch::naked_asm;
use osel_core::devices;
use osel_core::file::{Access, FdTable, OpenDescription};
use osel_core::errno::{Errno, KResult};
use osel_core::fpu::FpuState;
use osel_core::mmap::Touch;
use osel_core::trap::{GENERAL_PROTECTION, PAGE_FAULT, PF_FETCH, PF_PRESENT, PF_RESERVED, PF_WRITE, TrapFrame};

/// How a program's run ended
#[derive(Debug, Clone, Copy)]
pub enum Stop {
    /// `exit` or `exit_group` with this status
    Exit(u8),
    /// An exception the program caused
    Fault(Trap),
    /// `Process::kill`
    Killed,
}

impl Stop {
    /// The exception, panics if the program stopped otherwise
    #[track_caller]
    pub fn fault(self) -> Trap {
        match self {
            Stop::Fault(trap) => trap,
            other => panic!("the program stopped with {:?} instead of a fault", other),
        }
    }

    /// The exit status, panics if the program stopped otherwise
    #[track_caller]
    pub fn exit_code(self) -> u8 {
        match self {
            Stop::Exit(code) => code,
            other => panic!("the program stopped with {:?} instead of exiting", other),
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

/// Runs the program in `space` from `frame` until it exits, traps or is killed. The frame
/// then holds every register of the program at that moment. The frame is forced to ring 3
/// first; a `rip` outside the lower half is a #GP of the program right away, without
/// entering it.
pub fn run(space: &mut UserSpace, frame: &mut TrapFrame) -> Stop {
    run_with(space, &mut standard_files(), frame, &mut FpuState::initial())
}

/// Descriptors 0, 1 and 2 open on the terminal for reading and writing, like a shell
/// leaves them
pub fn standard_files() -> FdTable {
    let stat = devices::device_stat(devices::TTY).expect("no tty device");
    let tty = Arc::new(OpenDescription::new(Arc::new(Tty::new(stat)), Access { read: true, write: true }, 0));
    let mut files = FdTable::new();
    for _ in 0..3 {
        files.insert(tty.clone(), false).expect("an empty table has room");
    }
    files
}

/// `run` with the program's descriptors and its x87 and SSE state: the space and the
/// descriptors go to a new process for the run and come back after it, the state is the
/// thread's at the start and is saved when it stops. `run` starts every program with the
/// state after reset, so nothing of another program leaks in.
pub fn run_with(space: &mut UserSpace, files: &mut FdTable, frame: &mut TrapFrame, fpu: &mut FpuState) -> Stop {
    assert!(x86_64::instructions::interrupts::are_enabled(), "running a program with interrupts off or a lock held");
    let selectors = gdt::selectors();
    if !frame.make_user(selectors.user_code_selector.0, selectors.user_data_selector.0, USER_END) {
        frame.vector = GENERAL_PROTECTION;
        frame.error_code = 0;
        return Stop::Fault(Trap { vector: GENERAL_PROTECTION, error_code: 0, cr2: 0 });
    }
    let process = start(space.take(), core::mem::take(files), *frame, fpu.clone(), 0).expect("no kernel stack for the program");
    let exit = process.wait();
    *frame = exit.frame;
    *fpu = exit.fpu;
    // the thread is gone and dropped its reference when it was switched away from
    let (back_space, back_files) = Arc::into_inner(process).expect("the process is still referenced").into_parts();
    *space = back_space;
    *files = back_files;
    exit.stop
}

/// Starts a program in a new process without waiting for it: one thread from `frame`, with
/// its x87 and SSE state and FS base. The frame is forced to ring 3 as in `run`, but a
/// `rip` outside the lower half is `EFAULT` here. `ENOMEM` without a kernel stack.
pub fn start(space: UserSpace, files: FdTable, mut frame: TrapFrame, fpu: FpuState, fs_base: u64) -> KResult<Arc<Process>> {
    let selectors = gdt::selectors();
    if !frame.make_user(selectors.user_code_selector.0, selectors.user_data_selector.0, USER_END) {
        return Err(Errno::EFAULT);
    }
    let process = Process::new(space, files);
    scheduler::spawn_user(process.clone(), frame, fpu, fs_base)?;
    Ok(process)
}

/// Reached by a `jmp` from an entry stub after a trap from ring 3, with the vector and the
/// error code pushed on top of what the CPU pushed. Completes the `TrapFrame` at the top of
/// the kernel stack and calls `user_fault`.
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
        // the program may have left DF set
        "cld",
        "mov rdi, rsp",
        "call {user_fault}",
        "ud2",
        user_fault = sym user_fault,
    )
}

/// A fault of a program. A page fault on an untouched page of an area maps the page and
/// the program goes on; anything else ends it with the exception. Interrupts are still off
/// from the interrupt gate, so CR2 still holds the faulting address of a page fault.
extern "C" fn user_fault(frame: *const TrapFrame) -> ! {
    let cr2 = x86_64::registers::control::Cr2::read_raw();
    if unsafe { (*frame).vector } == PAGE_FAULT && map_on_demand(cr2, unsafe { (*frame).error_code }) {
        // a killed program goes no further, as after a system call
        if !scheduler::current_killed() {
            // the frame is at the top of the thread's kernel stack, where `return_to_user` wants it
            unsafe { scheduler::return_to_user(frame) }
        }
        scheduler::exit_current(Stop::Killed, Some(unsafe { *frame }));
    }
    let frame = unsafe { *frame };
    scheduler::exit_current(scheduler::fault_of(&frame, cr2), Some(frame))
}

/// Maps the page a program touched for the first time, see `Mappings::fault`. Only a not
/// present page of the lower half: a protection fault is the program's.
fn map_on_demand(address: u64, error_code: u64) -> bool {
    if error_code & (PF_PRESENT | PF_RESERVED) != 0 || address >= USER_END {
        return false;
    }
    let touch = if error_code & PF_FETCH != 0 {
        Touch::Execute
    } else if error_code & PF_WRITE != 0 {
        Touch::Write
    } else {
        Touch::Read
    };
    // SAFETY: the faulting thread is the process's only one, and it is here
    unsafe { scheduler::current_space() }.fault(address, touch)
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
        ".global osel_patterns", "osel_patterns:",
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
        ".global osel_patterns_fault", "osel_patterns_fault:",
        "mov qword ptr [0], rax",
        ".global osel_patterns_end", "osel_patterns_end:",
        // the start state: argc from the stack into rax, then fault
        ".global osel_start_state", "osel_start_state:",
        "mov rax, [rsp]",
        "mov qword ptr [0], rax",
        ".global osel_start_state_end", "osel_start_state_end:",
        // Nikka `user_mode_page_fault`: no stack at all
        ".global osel_no_stack", "osel_no_stack:",
        "xor esp, esp",
        "push rax",
        ".global osel_no_stack_end", "osel_no_stack_end:",
        // spins long enough for timer ticks even on fast hardware, with DF set, then faults
        ".global osel_spin", "osel_spin:",
        "std",
        "mov rcx, 200000000",
        "2:",
        "dec rcx",
        "jnz 2b",
        "mov qword ptr [0], rax",
        ".global osel_spin_end", "osel_spin_end:",
        ".global osel_hlt", "osel_hlt:", "hlt", ".global osel_hlt_end", "osel_hlt_end:",
        ".global osel_cli", "osel_cli:", "cli", ".global osel_cli_end", "osel_cli_end:",
        ".global osel_in", "osel_in:", "in al, 0x60", ".global osel_in_end", "osel_in_end:",
        ".global osel_int80", "osel_int80:", "int 0x80", ".global osel_int80_end", "osel_int80_end:",
        ".global osel_int3", "osel_int3:", "int3", ".global osel_int3_end", "osel_int3_end:",
        ".global osel_ud2", "osel_ud2:", "ud2", ".global osel_ud2_end", "osel_ud2_end:",
        ".global osel_wrmsr", "osel_wrmsr:", "xor eax, eax", "xor edx, edx", "mov ecx, 0xc0000080", "wrmsr", ".global osel_wrmsr_end", "osel_wrmsr_end:",
        ".global osel_rdmsr", "osel_rdmsr:", "mov ecx, 0xc0000080", "rdmsr", ".global osel_rdmsr_end", "osel_rdmsr_end:",
        ".global osel_out", "osel_out:", "out 0x80, al", ".global osel_out_end", "osel_out_end:",
        // calls itself until the stack runs into the guard page below it
        ".global osel_recursion", "osel_recursion:", "2:", "call 2b", ".global osel_recursion_end", "osel_recursion_end:",
        ".global osel_divide", "osel_divide:", "xor ecx, ecx", "div rcx", ".global osel_divide_end", "osel_divide_end:",
        ".global osel_kernel_jump", "osel_kernel_jump:", "mov rax, 0xffff800000000000", "jmp rax", ".global osel_kernel_jump_end", "osel_kernel_jump_end:",
        ".global osel_kernel_read", "osel_kernel_read:", "mov rax, 0xffff800000000000", "mov rax, [rax]", ".global osel_kernel_read_end", "osel_kernel_read_end:",
        ".global osel_write_text", "osel_write_text:", "lea rax, [rip]", "mov byte ptr [rax], 0", ".global osel_write_text_end", "osel_write_text_end:",
        ".global osel_run_data", "osel_run_data:", "mov rax, 0x600000", "jmp rax", ".global osel_run_data_end", "osel_run_data_end:",
        ".global osel_bad_stack", "osel_bad_stack:", "mov rsp, 0x800000000008", "push rax", ".global osel_bad_stack_end", "osel_bad_stack_end:",
        ".global osel_trap_flag", "osel_trap_flag:", "pushfq", "or qword ptr [rsp], 0x100", "popfq", "nop", "nop", ".global osel_trap_flag_end", "osel_trap_flag_end:",
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
        let frames = crate::memory::used_frames();
        for _ in 0..100 {
            let (_frame, trap, _loaded) = run_code(code!(osel_patterns, osel_patterns_end));
            assert_eq!(trap.vector, 14);
        }
        assert_eq!(crate::memory::used_frames(), frames);
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
