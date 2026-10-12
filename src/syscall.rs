//! syscall module is the `syscall` entry and the system calls the kernel implements
//!
//! Numbers, arguments and the choice of the way back live in `osel_core::syscall`; the
//! list of calls is in docs/reference/syscalls.md.

use crate::gdt::{self, USER_CODE_SELECTOR, USER_DATA_SELECTOR};
use crate::memory::USER_END;
use crate::user;
use crate::sync::IrqSpinlock;
use alloc::string::String;
use core::arch::naked_asm;
use osel_core::errno::{Errno, KResult};
use osel_core::syscall::{self, EXIT, EXIT_GROUP, GETPID, Return, SYSCALL_VECTOR, Utf8Decoder, WRITE};
use osel_core::user_mem::Access;
use osel_core::trap::TrapFrame;
use x86_64::VirtAddr;
use x86_64::registers::model_specific::{Efer, EferFlags, LStar, SFMask, Star};
use x86_64::registers::rflags::RFlags;

/// The program's stack pointer while the entry switches stacks
static mut USER_RSP: u64 = 0;

/// Text state of standard output and error: a character may be split between two `write`s
static OUTPUT_TEXT: IrqSpinlock<[Utf8Decoder; 2]> = IrqSpinlock::new("OUTPUT_TEXT", [Utf8Decoder::new(), Utf8Decoder::new()]);

/// Turns `syscall` on: selectors in STAR, the entry in LSTAR, and the flags the CPU clears
/// on entry. Interrupts stay off until the entry is on the kernel stack.
pub fn init() {
    let selectors = gdt::selectors();
    Star::write(selectors.user_code_selector, selectors.user_data_selector, selectors.code_selector, selectors.data_selector)
        .expect("the GDT order does not fit syscall");
    LStar::write(VirtAddr::new(syscall_entry as *const () as u64));
    SFMask::write(
        RFlags::INTERRUPT_FLAG
            | RFlags::DIRECTION_FLAG
            | RFlags::TRAP_FLAG
            | RFlags::ALIGNMENT_CHECK
            | RFlags::NESTED_TASK
            | RFlags::IOPL_HIGH
            | RFlags::IOPL_LOW,
    );
    unsafe { Efer::update(|flags| flags.insert(EferFlags::SYSTEM_CALL_EXTENSIONS)) };
}

/// The CPU jumps here on `syscall` with the program's stack, RCX = return address and
/// R11 = the program's flags. Builds a `TrapFrame` on the kernel stack the run left, turns
/// interrupts on, lets `dispatch` handle the call, then goes back with `sysretq`, `iretq`
/// or leaves the run.
#[unsafe(naked)]
unsafe extern "C" fn syscall_entry() {
    naked_asm!(
        "mov [rip + {user_rsp}], rsp",
        "mov rsp, [rip + {kernel_rsp}]",
        // labels for the test of the order: stack first, then interrupts
        ".global osel_syscall_on_kernel_stack",
        "osel_syscall_on_kernel_stack:",
        // the frame below has an even number of words: `call` then sees an aligned stack
        "and rsp, -16",
        "push {user_data}",
        "push qword ptr [rip + {user_rsp}]",
        "push r11",
        "push {user_code}",
        "push rcx",
        // error code and vector
        "push 0",
        "push {vector}",
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
        // only now, on the kernel stack: an interrupt before would push onto the program's
        ".global osel_syscall_sti",
        "osel_syscall_sti:",
        "sti",
        "mov rdi, rsp",
        "call {dispatch}",
        "cli",
        "cmp rax, {leave}",
        "je {leave_user}",
        "cmp rax, {iret}",
        "je 2f",
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
        // vector, error code, then rip, cs, rflags, rsp as `dispatch` left them
        "mov rcx, [rsp + 16]",
        "mov r11, [rsp + 32]",
        "mov rsp, [rsp + 40]",
        "sysretq",
        "2:",
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
        "add rsp, 16",
        "iretq",
        user_rsp = sym USER_RSP,
        kernel_rsp = sym user::KERNEL_RSP,
        user_data = const USER_DATA_SELECTOR,
        user_code = const USER_CODE_SELECTOR,
        vector = const SYSCALL_VECTOR,
        dispatch = sym dispatch,
        leave = const Return::Leave as u64,
        iret = const Return::Iret as u64,
        leave_user = sym user::leave_user,
    )
}

/// Runs the call the program asked for with interrupts on, puts the result into RAX of the
/// frame and says how to go back
extern "C" fn dispatch(frame: &mut TrapFrame) -> u64 {
    let [a0, a1, a2, ..] = syscall::args(frame);
    let result = match syscall::number(frame) {
        Some(WRITE) => write(a0, a1, a2),
        Some(GETPID) => Ok(1),
        // the run ends; `user::run` takes the status from RDI
        Some(EXIT | EXIT_GROUP) => return Return::Leave as u64,
        _ => Err(Errno::ENOSYS),
    };
    frame.rax = syscall::encode(result);
    syscall::return_path(frame, USER_CODE_SELECTOR, USER_DATA_SELECTOR, USER_END) as u64
}

/// `write(fd, buf, count)`: standard output and error go to the screen and the serial port
fn write(fd: u64, buf: u64, count: u64) -> KResult<u64> {
    if fd != 1 && fd != 2 {
        return Err(Errno::EBADF);
    }
    // SAFETY: called from `dispatch`, a program is running
    let space = unsafe { user::running_space() };
    let count = usize::try_from(count).map_err(|_| Errno::EFAULT)?;
    // all of it or nothing: a bad buffer prints no part of it
    space.check_user(buf, count, Access::Read)?;
    let mut chunk = [0u8; 256];
    let mut done = 0;
    while done < count {
        let len = (count - done).min(chunk.len());
        space.copy_from_user(buf + done as u64, &mut chunk[..len])?;
        let mut text = String::new();
        // decoded under the lock, printed after it: printing takes the console locks
        OUTPUT_TEXT.lock()[fd as usize - 1].decode(&chunk[..len], |piece| text.push_str(piece));
        crate::print!("{}", text);
        crate::serial_print!("{}", text);
        done += len;
    }
    Ok(count as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{self, UserSpace};
    use crate::user::tests::{TEXT, code, kernel_is_fine, run_program};
    use crate::user::{Stop, initial_frame, run};
    use core::arch::global_asm;
    use x86_64::structures::paging::PageTableFlags;

    global_asm!(
        ".pushsection .rodata.osel_syscall_programs, \"a\"",
        // write(1, "hello\n", 6), then exit with what write returned
        "osel_hello:",
        "mov edi, 1",
        "lea rsi, [rip + 3f]",
        "mov edx, 6",
        "mov eax, 1",
        "syscall",
        "mov rdi, rax",
        "mov eax, 60",
        "syscall",
        "3:",
        ".ascii \"hello\\n\"",
        "osel_hello_end:",
        // every register gets a pattern, getpid, then a fault shows what came back
        "osel_getpid:",
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
        "mov eax, 39",
        "syscall",
        "osel_getpid_after:",
        "mov qword ptr [0], rax",
        "osel_getpid_end:",
        // write(1, rsi, rdx) with rsi and rdx from the test, exit with the result
        "osel_write_args:",
        "mov edi, 1",
        "mov eax, 1",
        "syscall",
        "mov rdi, rax",
        "mov eax, 60",
        "syscall",
        "osel_write_args_end:",
        // any call with rax, rdi from the test, exit with the result
        "osel_call:",
        "syscall",
        "mov rdi, rax",
        "mov eax, 60",
        "syscall",
        "osel_call_end:",
        // Nikka `syscall_exit`: no stack while making calls; ticks land in some of them
        "osel_no_stack_calls:",
        "xor esp, esp",
        "mov r12d, 200000",
        "2:",
        "mov eax, 39",
        "syscall",
        "dec r12",
        "jnz 2b",
        "xor edi, edi",
        "mov eax, 60",
        "syscall",
        "osel_no_stack_calls_end:",
        // a call with DF and AC set: both come back, the kernel ran without them
        "osel_flags:",
        "std",
        "pushfq",
        "or qword ptr [rsp], 0x40000",
        "popfq",
        "mov eax, 39",
        "syscall",
        "pushfq",
        "pop rbx",
        "mov qword ptr [0], rax",
        "osel_flags_end:",
        // a call made with the trap flag set
        "osel_traced:",
        "pushfq",
        "or qword ptr [rsp], 0x100",
        "mov eax, 39",
        "popfq",
        "syscall",
        "nop",
        "nop",
        "osel_traced_end:",
        ".popsection",
    );

    /// Runs `code` with `rax`, `rdi`, `rsi`, `rdx` set by the kernel before entering
    fn run_with(code: &[u8], rax: u64, rdi: u64, rsi: u64, rdx: u64) -> Stop {
        let mut loaded = crate::user::tests::load(code);
        let mut frame = initial_frame(loaded.entry, loaded.rsp);
        (frame.rax, frame.rdi, frame.rsi, frame.rdx) = (rax, rdi, rsi, rdx);
        run(&mut loaded.space, &mut frame)
    }

    fn errno(errno: Errno) -> u8 {
        (-(errno.code())) as u8
    }

    #[test_case]
    fn test_hello() {
        let (_frame, stop, _loaded) = run_program(code!(osel_hello, osel_hello_end));
        assert_eq!(stop.exit_code(), 6, "write returns the bytes written");
        kernel_is_fine();
        let shown = crate::avganec::with_console(|console| {
            let row = console.cursor().0 - 1;
            "hello".chars().enumerate().all(|(i, c)| console.cell_shows(row, i, c))
        });
        assert_ne!(shown, Some(false), "hello is not on the screen");
    }

    /// The design's check: after `getpid` only RAX, RCX and R11 differ
    #[test_case]
    fn test_a_call_keeps_the_registers() {
        let code = code!(osel_getpid, osel_getpid_end);
        let after = code!(osel_getpid, osel_getpid_after).len() as u64;
        let (frame, stop, _loaded) = run_program(code);
        assert_eq!(stop.fault().vector, 14);
        assert_eq!(frame.rax, 1, "the pid");
        assert_eq!(frame.rcx, TEXT + after, "RCX: where the call returned to");
        assert_eq!(frame.r11 & !(1 << 16), 0x202, "R11: the program's flags");
        let kept = [
            frame.rbx, frame.rdx, frame.rsi, frame.rdi, frame.rbp, frame.r8, frame.r9, frame.r10, frame.r12, frame.r13,
            frame.r14, frame.r15,
        ];
        let expected = [2u64, 4, 5, 6, 7, 8, 9, 10, 12, 13, 14, 15].map(|n| n * 0x0101_0101_0101_0101);
        assert_eq!(kept, expected);
    }

    #[test_case]
    fn test_bad_buffers_are_efault() {
        let code = code!(osel_write_args, osel_write_args_end);
        let kernel = VirtAddr::from_ptr(&SYSCALL_VECTOR).as_u64();
        for (name, buf, len) in [
            ("null", 0, 1),
            ("kernel memory", kernel, 8),
            ("not mapped", 0x50_0000, 1),
            ("into the unmapped page after the code", TEXT + 0xffe, 4),
            ("overflowing", u64::MAX - 1, 4),
        ] {
            assert_eq!(run_with(code, 0, 0, buf, len).exit_code(), errno(Errno::EFAULT), "{}", name);
        }
        assert_eq!(run_with(code, 0, 0, TEXT, 0).exit_code(), 0, "nothing to write");
        assert_eq!(run_with(code, 0, 0, 0, 0).exit_code(), 0, "nothing to write from NULL, like Linux");
        kernel_is_fine();
    }

    #[test_case]
    fn test_a_bad_buffer_prints_nothing() {
        let code = code!(osel_write_args, osel_write_args_end);
        let cursor = || crate::avganec::with_console(|console| console.cursor());
        let before = cursor();
        // "hello" then the unmapped page: not even the readable part may come out
        assert_eq!(run_with(code, 0, 0, TEXT + 0xffe, 4).exit_code(), errno(Errno::EFAULT));
        assert_eq!(cursor(), before);
    }

    #[test_case]
    fn test_unknown_calls_and_descriptors() {
        let call = code!(osel_call, osel_call_end);
        assert_eq!(run_with(call, 9999, 0, 0, 0).exit_code(), errno(Errno::ENOSYS));
        assert_eq!(run_with(call, u64::MAX, 0, 0, 0).exit_code(), errno(Errno::ENOSYS), "-1");
        assert_eq!(run_with(call, 0x1_0000_0000 | GETPID, 0, 0, 0).exit_code(), 1, "only the low 32 bits count");
        assert_eq!(run_with(call, EXIT, 256, 0, 0).exit_code(), 0, "the status is 8 bits");
        assert_eq!(run_with(call, EXIT_GROUP, 7, 0, 0).exit_code(), 7);
        for fd in [0, 3, u64::MAX] {
            assert_eq!(run_with(call, WRITE, fd, TEXT, 1).exit_code(), errno(Errno::EBADF), "fd {}", fd);
        }
    }

    #[test_case]
    fn test_calls_without_a_stack() {
        let before = crate::interrupts::ticks();
        let (_frame, stop, _loaded) = run_program(code!(osel_no_stack_calls, osel_no_stack_calls_end));
        assert_eq!(stop.exit_code(), 0);
        kernel_is_fine();
        assert!(crate::interrupts::ticks() > before, "no timer interrupt during the calls");
    }

    #[test_case]
    fn test_the_program_keeps_its_flags_across_a_call() {
        let (frame, stop, _loaded) = run_program(code!(osel_flags, osel_flags_end));
        assert_eq!(stop.fault().vector, 14);
        kernel_is_fine();
        assert!(frame.rbx & (1 << 10) != 0, "DF");
        assert!(frame.rbx & (1 << 18) != 0, "AC");
    }

    #[test_case]
    fn test_a_traced_call() {
        let (_frame, stop, _loaded) = run_program(code!(osel_traced, osel_traced_end));
        assert_eq!(stop.fault().vector, 1, "the single step trap reaches the program");
        kernel_is_fine();
    }

    /// Linux `sysret_rip`: a call in the last bytes of the lower half returns to a
    /// non-canonical address. The program ends with #GP, the kernel survives.
    #[test_case]
    fn test_a_call_at_the_end_of_the_lower_half() {
        let page = USER_END - 4096;
        let code_flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
        // mov eax, 39; syscall — and once more with a nop after it
        for (tail, vectors) in [(&[0xb8, 39, 0, 0, 0, 0x0f, 0x05][..], &[13][..]), (&[0xb8, 39, 0, 0, 0, 0x0f, 0x05, 0x90][..], &[13, 14][..])] {
            let mut space = UserSpace::new().unwrap();
            space.map(VirtAddr::new(page), 4096, code_flags).unwrap();
            let start = USER_END - tail.len() as u64;
            space.write_bytes(start, tail).unwrap();
            let mut frame = initial_frame(start, 0);
            let trap = run(&mut space, &mut frame).fault();
            assert!(vectors.contains(&trap.vector), "{} bytes: vector {}", tail.len(), trap.vector);
            kernel_is_fine();
        }
    }

    /// The entry turns interrupts on only after it is on the kernel stack. A timer can't be
    /// made to hit that window on purpose, so the order is checked in the machine code.
    #[test_case]
    fn test_interrupts_come_on_after_the_stack_switch() {
        unsafe extern "C" {
            static osel_syscall_on_kernel_stack: u8;
            static osel_syscall_sti: u8;
        }
        let entry = syscall_entry as *const u8;
        let on_kernel_stack = core::ptr::addr_of!(osel_syscall_on_kernel_stack);
        let sti = core::ptr::addr_of!(osel_syscall_sti);
        assert!(entry < on_kernel_stack && on_kernel_stack < sti, "the stack switch is not before sti");
        assert_eq!(unsafe { sti.read() }, 0xfb, "the label is not on sti");
        // the stack switch is the second instruction, nothing else is in between
        let code = unsafe { core::slice::from_raw_parts(entry, 14) };
        assert_eq!(&code[..3], [0x48, 0x89, 0x25], "mov [rip + ...], rsp");
        assert_eq!(&code[7..10], [0x48, 0x8b, 0x25], "mov rsp, [rip + ...]");
        assert_eq!(unsafe { entry.add(14) }, on_kernel_stack);
    }

    /// A character split between two `write`s comes out whole, as on a Linux terminal
    #[test_case]
    fn test_a_character_split_between_writes() {
        let code = code!(osel_write_args, osel_write_args_end);
        let mut loaded = crate::user::tests::load(code);
        // the line ends, so later tests find their output at the start of a line
        let text = "ж\n".as_bytes();
        loaded.space.map(VirtAddr::new(0x70_0000), 4096, PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::WRITABLE).unwrap();
        loaded.space.write_bytes(0x70_0000, text).unwrap();
        // the font has no Cyrillic, so the decoder itself shows that the halves met
        for (at, pending_after) in [(0x70_0000, true), (0x70_0001, false)] {
            let mut frame = initial_frame(loaded.entry, loaded.rsp);
            (frame.rsi, frame.rdx) = (at, 1);
            assert_eq!(run(&mut loaded.space, &mut frame).exit_code(), 1);
            assert_eq!(OUTPUT_TEXT.lock()[0].is_pending(), pending_after);
        }
        let mut frame = initial_frame(loaded.entry, loaded.rsp);
        (frame.rsi, frame.rdx) = (0x70_0002, 1);
        assert_eq!(run(&mut loaded.space, &mut frame).exit_code(), 1);
    }

    #[test_case]
    fn test_many_runs_leak_nothing() {
        let frames = memory::used_frames();
        for _ in 0..100 {
            let stop = run_with(code!(osel_call, osel_call_end), GETPID, 0, 0, 0);
            assert_eq!(stop.exit_code(), 1);
        }
        assert_eq!(memory::used_frames(), frames);
    }
}
