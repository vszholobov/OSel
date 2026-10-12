//! syscall module is the `syscall` entry and the system calls the kernel implements
//!
//! Numbers, arguments and the choice of the way back live in `osel_core::syscall`; the
//! list of calls is in docs/reference/syscalls.md.

use crate::gdt::{self, USER_CODE_SELECTOR, USER_DATA_SELECTOR};
use crate::memory::USER_END;
use crate::user;
use alloc::string::String;
use core::arch::naked_asm;
use osel_core::errno::{Errno, KResult};
use crate::fs;
use crate::memory::UserSpace;
use alloc::sync::Arc;
use alloc::vec;
use osel_core::file::{FdTable, O_CLOEXEC, O_CREAT, OpenDescription, encode_stat, open_access};
use osel_core::syscall::{
    self, CLOSE, EXIT, EXIT_GROUP, FCNTL, FSTAT, GETPID, NEWFSTATAT, OPENAT, READ, Return, SYSCALL_VECTOR, WRITE, fd_arg,
};
use osel_core::user_mem::Access as MemAccess;
use osel_core::vfs::{FileType, Inode, PATH_MAX, resolve};
use osel_core::trap::TrapFrame;
use x86_64::VirtAddr;
use x86_64::registers::model_specific::{Efer, EferFlags, LStar, SFMask, Star};
use x86_64::registers::rflags::RFlags;

/// The program's stack pointer while the entry switches stacks
static mut USER_RSP: u64 = 0;


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
        ".global osel_syscall_on_kernel_stack", "osel_syscall_on_kernel_stack:",
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
        ".global osel_syscall_sti", "osel_syscall_sti:",
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
    let [a0, a1, a2, a3, ..] = syscall::args(frame);
    let result = match syscall::number(frame) {
        Some(READ) => read(fd_arg(a0), a1, a2),
        Some(WRITE) => write(fd_arg(a0), a1, a2),
        Some(OPENAT) => openat(fd_arg(a0), a1, a2 as u32),
        Some(CLOSE) => files().close(fd_arg(a0)).map(|()| 0),
        Some(FSTAT) => fstat(fd_arg(a0), a1),
        Some(NEWFSTATAT) => newfstatat(fd_arg(a0), a1, a2, a3 as u32),
        Some(FCNTL) => fcntl(fd_arg(a0), a1 as u32, a2),
        Some(GETPID) => Ok(1),
        // the run ends; `user::run` takes the status from RDI
        Some(EXIT | EXIT_GROUP) => return Return::Leave as u64,
        _ => Err(Errno::ENOSYS),
    };
    frame.rax = syscall::encode(result);
    syscall::return_path(frame, USER_CODE_SELECTOR, USER_DATA_SELECTOR, USER_END) as u64
}

fn space() -> &'static mut UserSpace {
    // SAFETY: only called from `dispatch`, a program is running
    unsafe { user::running_space() }
}

fn files() -> &'static mut FdTable {
    // SAFETY: as `space`
    unsafe { user::running_files() }
}

/// Bytes one `read` moves at most; a program reads again for the rest, as with a pipe
const MAX_READ: usize = 64 * 1024;
/// `dirfd` meaning the current directory, which is `/` for every program yet
const AT_FDCWD: i32 = -100;
/// `newfstatat` of `dirfd` itself when the path is empty
const AT_EMPTY_PATH: u32 = 0x1000;

const F_DUPFD: u32 = 0;
const F_GETFD: u32 = 1;
const F_SETFD: u32 = 2;
const F_GETFL: u32 = 3;
const F_SETFL: u32 = 4;
const F_DUPFD_CLOEXEC: u32 = 1030;
const FD_CLOEXEC: u64 = 1;

/// `read(fd, buf, count)`
fn read(fd: i32, buf: u64, count: u64) -> KResult<u64> {
    let description = files().get(fd)?;
    let count = usize::try_from(count).unwrap_or(usize::MAX).min(MAX_READ);
    // checked before anything is taken from the file: a typed key must not get lost
    space().check_user(buf, count, MemAccess::Write)?;
    let mut data = vec![0u8; count];
    let read = description.read(&mut data)?;
    space().copy_to_user(buf, &data[..read])?;
    Ok(read as u64)
}

/// `write(fd, buf, count)`
fn write(fd: i32, buf: u64, count: u64) -> KResult<u64> {
    let description = files().get(fd)?;
    let count = usize::try_from(count).map_err(|_| Errno::EFAULT)?;
    // all of it or nothing: a bad buffer writes no part of it
    space().check_user(buf, count, MemAccess::Read)?;
    let mut chunk = [0u8; 256];
    let mut done = 0;
    while done < count {
        let len = (count - done).min(chunk.len());
        space().copy_from_user(buf + done as u64, &mut chunk[..len])?;
        // like Linux: what was written counts, the error only if nothing was
        let written = match description.write(&chunk[..len]) {
            Ok(written) => written,
            Err(_) if done > 0 => break,
            Err(error) => return Err(error),
        };
        done += written;
        if written < len {
            break;
        }
    }
    Ok(done as u64)
}

/// A path a program passed: up to `PATH_MAX` bytes with the NUL. Bytes that are not
/// UTF-8 can't name anything in OSel's file systems.
fn path_from_user(address: u64) -> KResult<String> {
    let bytes = space().copy_string_from_user(address, PATH_MAX)?;
    String::from_utf8(bytes).map_err(|_| Errno::ENOENT)
}

/// The directory a relative `path` starts from: `dirfd` or the current directory
fn start_dir(dirfd: i32, path: &str, root: &Arc<dyn Inode>) -> KResult<Arc<dyn Inode>> {
    if path.starts_with('/') || dirfd == AT_FDCWD {
        return Ok(root.clone());
    }
    let inode = files().get(dirfd)?.file().inode().ok_or(Errno::ENOTDIR)?;
    if inode.stat().file_type != FileType::Directory {
        return Err(Errno::ENOTDIR);
    }
    Ok(inode)
}

/// `openat(dirfd, path, flags, mode)`
fn openat(dirfd: i32, path: u64, flags: u32) -> KResult<u64> {
    let path = path_from_user(path)?;
    if path.is_empty() {
        return Err(Errno::ENOENT);
    }
    let fs = fs::root().ok_or(Errno::ENOENT)?;
    let root = fs.root();
    let start = start_dir(dirfd, &path, &root)?;
    let node = match resolve(&root, &start, &path) {
        Ok(node) => node,
        // the last name is missing: creating it fails on a read-only file system, and a
        // name with a trailing slash can only be a directory, which open never creates
        Err(Errno::ENOENT) if flags & O_CREAT != 0 && parent_exists(&root, &start, &path) => {
            if path.ends_with('/') {
                return Err(Errno::EISDIR);
            }
            return Err(open_access(flags, None).expect_err("nothing to open"));
        }
        Err(error) => return Err(error),
    };
    let access = open_access(flags, Some(node.stat().file_type))?;
    let description = Arc::new(OpenDescription::new(fs::file_for(node)?, access, flags));
    Ok(files().insert(description, flags & O_CLOEXEC != 0)? as u64)
}

/// Whether the directory that would hold the last name of `path` exists
fn parent_exists(root: &Arc<dyn Inode>, start: &Arc<dyn Inode>, path: &str) -> bool {
    let trimmed = path.trim_end_matches('/');
    let parent = match trimmed.rfind('/') {
        Some(0) => "/",
        Some(at) => &trimmed[..at],
        None => ".",
    };
    resolve(root, start, parent).is_ok_and(|dir| dir.stat().file_type == FileType::Directory)
}

/// `fstat(fd, statbuf)`
fn fstat(fd: i32, statbuf: u64) -> KResult<u64> {
    let stat = files().get(fd)?.stat();
    space().copy_to_user(statbuf, &encode_stat(&stat))?;
    Ok(0)
}

/// `newfstatat(dirfd, path, statbuf, flags)`: `stat` and `fstatat` of a path, or of `dirfd`
/// itself with `AT_EMPTY_PATH`. There are no symlinks, so `AT_SYMLINK_NOFOLLOW` changes nothing.
fn newfstatat(dirfd: i32, path: u64, statbuf: u64, flags: u32) -> KResult<u64> {
    let path = path_from_user(path)?;
    let fs = fs::root().ok_or(Errno::ENOENT)?;
    let root = fs.root();
    if path.is_empty() && flags & AT_EMPTY_PATH == 0 {
        return Err(Errno::ENOENT);
    }
    let stat = if path.is_empty() {
        match dirfd {
            AT_FDCWD => root.stat(),
            fd => files().get(fd)?.stat(),
        }
    } else {
        resolve(&root, &start_dir(dirfd, &path, &root)?, &path)?.stat()
    };
    space().copy_to_user(statbuf, &encode_stat(&stat))?;
    Ok(0)
}

/// `fcntl(fd, cmd, arg)`: duplicating, the close-on-exec flag and the status flags
fn fcntl(fd: i32, cmd: u32, arg: u64) -> KResult<u64> {
    let files = files();
    // the descriptor is checked before the command, as Linux does
    files.get(fd)?;
    match cmd {
        F_DUPFD | F_DUPFD_CLOEXEC => Ok(files.dup(fd, fd_arg(arg), cmd == F_DUPFD_CLOEXEC)? as u64),
        F_GETFD => Ok(if files.cloexec(fd)? { FD_CLOEXEC } else { 0 }),
        F_SETFD => files.set_cloexec(fd, arg & FD_CLOEXEC != 0).map(|()| 0),
        F_GETFL => Ok(files.get(fd)?.flags() as u64),
        F_SETFL => {
            files.get(fd)?.set_status(arg as u32);
            Ok(0)
        }
        _ => Err(Errno::EINVAL),
    }
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
        ".global osel_hello", "osel_hello:",
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
        ".global osel_hello_end", "osel_hello_end:",
        // every register gets a pattern, getpid, then a fault shows what came back
        ".global osel_getpid", "osel_getpid:",
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
        ".global osel_getpid_after", "osel_getpid_after:",
        "mov qword ptr [0], rax",
        ".global osel_getpid_end", "osel_getpid_end:",
        // write(1, rsi, rdx) with rsi and rdx from the test, exit with the result
        ".global osel_write_args", "osel_write_args:",
        "mov edi, 1",
        "mov eax, 1",
        "syscall",
        "mov rdi, rax",
        "mov eax, 60",
        "syscall",
        ".global osel_write_args_end", "osel_write_args_end:",
        // any call with rax, rdi from the test, exit with the result
        ".global osel_call", "osel_call:",
        "syscall",
        "mov rdi, rax",
        "mov eax, 60",
        "syscall",
        ".global osel_call_end", "osel_call_end:",
        // Nikka `syscall_exit`: no stack while making calls; ticks land in some of them
        ".global osel_no_stack_calls", "osel_no_stack_calls:",
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
        ".global osel_no_stack_calls_end", "osel_no_stack_calls_end:",
        // a call with DF and AC set: both come back, the kernel ran without them
        ".global osel_flags", "osel_flags:",
        "std",
        "pushfq",
        "or qword ptr [rsp], 0x40000",
        "popfq",
        "mov eax, 39",
        "syscall",
        "pushfq",
        "pop rbx",
        "mov qword ptr [0], rax",
        ".global osel_flags_end", "osel_flags_end:",
        // a call made with the trap flag set
        ".global osel_traced", "osel_traced:",
        "pushfq",
        "or qword ptr [rsp], 0x100",
        "mov eax, 39",
        "popfq",
        "syscall",
        "nop",
        "nop",
        ".global osel_traced_end", "osel_traced_end:",
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
        for fd in [3, 99, 1024, u64::MAX] {
            assert_eq!(run_with(call, WRITE, fd, TEXT, 1).exit_code(), errno(Errno::EBADF), "fd {:#x}", fd);
        }
        // standard input is the terminal, open for writing too, as a shell leaves it
        assert_eq!(run_with(call, WRITE, 0, TEXT, 0).exit_code(), 0);
        assert_eq!(run_with(call, WRITE, 0xffff_ffff_0000_0002, TEXT, 0).exit_code(), 0, "only the low 32 bits of fd count");
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
            assert_eq!(crate::tty::output_pending(), pending_after);
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

#[cfg(test)]
mod file_tests {
    use super::*;
    use crate::user::tests::{code, kernel_is_fine, load};
    use crate::user::{Stop, initial_frame, run};
    use core::arch::global_asm;
    use osel_core::file::{O_DIRECTORY, O_RDWR, O_TRUNC, O_WRONLY};
    use x86_64::VirtAddr;
    use x86_64::structures::paging::PageTableFlags;

    /// The data page every test program has, see `user::tests::load`
    const DATA: u64 = 0x60_0000;
    const BUFFER: u64 = DATA + 0x400;
    const STAT_BUFFER: u64 = DATA + 0x100;
    /// Go loads `AT_FDCWD` with a 32-bit move
    const GO_AT_FDCWD: u64 = 0xffff_ff9c;

    global_asm!(
        ".pushsection .rodata.osel_file_programs, \"a\"",
        // any call with the registers the test set, exit with the result
        ".global osel_any_call", "osel_any_call:",
        "syscall",
        "mov rdi, rax",
        "mov eax, 60",
        "syscall",
        ".global osel_any_call_end", "osel_any_call_end:",
        // openat with the registers the test set, then read r12 bytes into BUFFER
        ".global osel_open_read", "osel_open_read:",
        "mov eax, 257",
        "syscall",
        "test rax, rax",
        "js 2f",
        "mov rdi, rax",
        "mov esi, 0x600400",
        "mov rdx, r12",
        "xor eax, eax",
        "syscall",
        "2:",
        "mov rdi, rax",
        "mov eax, 60",
        "syscall",
        ".global osel_open_read_end", "osel_open_read_end:",
        // cat: openat, read into BUFFER, write that to fd 1, close, exit with the size
        ".global osel_cat", "osel_cat:",
        "mov eax, 257",
        "syscall",
        "mov r12, rax",
        "mov rdi, rax",
        "mov esi, 0x600400",
        "mov edx, 2048",
        "xor eax, eax",
        "syscall",
        "mov r13, rax",
        "mov edi, 1",
        "mov esi, 0x600400",
        "mov rdx, r13",
        "mov eax, 1",
        "syscall",
        "mov rdi, r12",
        "mov eax, 3",
        "syscall",
        "mov rdi, r13",
        "mov eax, 60",
        "syscall",
        ".global osel_cat_end", "osel_cat_end:",
        // openat, fstat into STAT_BUFFER, exit with fstat's result
        ".global osel_open_fstat", "osel_open_fstat:",
        "mov eax, 257",
        "syscall",
        "mov rdi, rax",
        "mov esi, 0x600100",
        "mov eax, 5",
        "syscall",
        "mov rdi, rax",
        "mov eax, 60",
        "syscall",
        ".global osel_open_fstat_end", "osel_open_fstat_end:",
        // close(0) twice, exit with the second result
        ".global osel_close_twice", "osel_close_twice:",
        "xor edi, edi",
        "mov eax, 3",
        "syscall",
        "xor edi, edi",
        "mov eax, 3",
        "syscall",
        "mov rdi, rax",
        "mov eax, 60",
        "syscall",
        ".global osel_close_twice_end", "osel_close_twice_end:",
        // opens r12 files and exits without closing them
        ".global osel_open_many", "osel_open_many:",
        "2:",
        "mov edi, 0xffffff9c",
        "mov esi, 0x600000",
        "xor edx, edx",
        "mov eax, 257",
        "syscall",
        "dec r12",
        "jnz 2b",
        "xor edi, edi",
        "mov eax, 60",
        "syscall",
        ".global osel_open_many_end", "osel_open_many_end:",
        ".popsection",
    );

    /// Runs `code` with `path` at the start of the data page and registers set by `setup`
    fn run_program(code: &[u8], path: &[u8], setup: impl FnOnce(&mut TrapFrame)) -> (TrapFrame, Stop, crate::program::Loaded) {
        let mut loaded = load(code);
        loaded.space.write_bytes(DATA, path).unwrap();
        loaded.space.write_bytes(DATA + path.len() as u64, &[0]).unwrap();
        let mut frame = initial_frame(loaded.entry, loaded.rsp);
        setup(&mut frame);
        let stop = run(&mut loaded.space, &mut frame);
        (frame, stop, loaded)
    }

    /// `openat(AT_FDCWD, path, flags)` and exit with the result
    fn open(path: &[u8], flags: u32) -> u8 {
        let (_frame, stop, _loaded) = run_program(code!(osel_any_call, osel_any_call_end), path, |frame| {
            (frame.rax, frame.rdi, frame.rsi, frame.rdx) = (OPENAT, GO_AT_FDCWD, DATA, flags as u64);
        });
        stop.exit_code()
    }

    fn errno(errno: Errno) -> u8 {
        (-(errno.code())) as u8
    }

    fn motd() -> alloc::vec::Vec<u8> {
        fs::read_file("/etc/motd").expect("no /etc/motd in the ramdisk")
    }

    #[test_case]
    fn test_cat() {
        let motd = motd();
        let (_frame, stop, mut loaded) = run_program(code!(osel_cat, osel_cat_end), b"/etc/motd", |frame| {
            (frame.rdi, frame.rsi, frame.rdx) = (GO_AT_FDCWD, DATA, 0);
        });
        assert_eq!(stop.exit_code(), motd.len() as u8);
        let mut read = alloc::vec![0u8; motd.len()];
        loaded.space.read_bytes(BUFFER, &mut read).unwrap();
        assert_eq!(read, motd);
        kernel_is_fine();
    }

    #[test_case]
    fn test_fstat_reports_the_file() {
        let (_frame, stop, mut loaded) = run_program(code!(osel_open_fstat, osel_open_fstat_end), b"/etc/motd", |frame| {
            (frame.rdi, frame.rsi, frame.rdx) = (GO_AT_FDCWD, DATA, 0);
        });
        assert_eq!(stop.exit_code(), 0);
        let mut stat = [0u8; 144];
        loaded.space.read_bytes(STAT_BUFFER, &mut stat).unwrap();
        assert_eq!(u64::from_le_bytes(stat[48..56].try_into().unwrap()), motd().len() as u64, "st_size");
        assert_eq!(u32::from_le_bytes(stat[24..28].try_into().unwrap()) & 0o170000, 0o100000, "a regular file");
    }

    #[test_case]
    fn test_open_errors_follow_linux() {
        assert_eq!(open(b"/etc/motd", 0), 3, "the lowest free descriptor after 0, 1, 2");
        assert_eq!(open(b"etc/motd", 0), 3, "relative to the current directory, /");
        assert_eq!(open(b"/no/such", 0), errno(Errno::ENOENT));
        assert_eq!(open(b"/etc/nope", 0), errno(Errno::ENOENT));
        assert_eq!(open(b"", 0), errno(Errno::ENOENT));
        assert_eq!(open(b"/etc/motd", O_WRONLY), errno(Errno::EROFS));
        assert_eq!(open(b"/etc/new", O_CREAT | O_WRONLY), errno(Errno::EROFS));
        assert_eq!(open(b"/nope/new", O_CREAT | O_WRONLY), errno(Errno::ENOENT), "no directory to create it in");
        assert_eq!(open(b"/etc/motd", O_DIRECTORY), errno(Errno::ENOTDIR));
        assert_eq!(open(b"/etc", O_RDWR), errno(Errno::EISDIR));
        assert_eq!(open(b"/etc/motd/x", 0), errno(Errno::ENOTDIR));
        assert_eq!(open(b"/etc/motd/", 0), errno(Errno::ENOTDIR), "a trailing slash needs a directory");
        assert_eq!(open(b"/etc/new/", O_CREAT), errno(Errno::EISDIR));
        assert_eq!(open(b"/etc", O_TRUNC), errno(Errno::EISDIR));
        assert_eq!(open(b"/dev/null", O_RDWR), 3);
        assert_eq!(open(b"//dev/./tty", O_RDWR), 3);
        assert_eq!(open(b"/dev/nope", 0), errno(Errno::ENOENT));
        assert_eq!(open(b"/dev/null", O_DIRECTORY), errno(Errno::ENOTDIR));
        assert_eq!(open(b"/sys/kernel/mm/transparent_hugepage/hpage_pmd_size", 0), errno(Errno::ENOENT), "Go opens it at start");
        assert_eq!(open(b"/\xff", 0), errno(Errno::ENOENT), "not UTF-8");
        kernel_is_fine();
    }

    #[test_case]
    fn test_paths_are_read_carefully() {
        let call = code!(osel_any_call, osel_any_call_end);
        let open_at = |address: u64, prepare: &dyn Fn(&mut UserSpace)| {
            let mut loaded = load(call);
            prepare(&mut loaded.space);
            let mut frame = initial_frame(loaded.entry, loaded.rsp);
            (frame.rax, frame.rdi, frame.rsi, frame.rdx) = (OPENAT, GO_AT_FDCWD, address, 0);
            run(&mut loaded.space, &mut frame).exit_code()
        };
        assert_eq!(open_at(0, &|_| {}), errno(Errno::EFAULT), "NULL");
        let kernel = VirtAddr::from_ptr(&MAX_READ).as_u64();
        assert_eq!(open_at(kernel, &|_| {}), errno(Errno::EFAULT), "kernel memory");
        // a path ending right before an unmapped page is fine
        let end = DATA + 4096;
        assert_eq!(open_at(end - 10, &|space| space.write_bytes(end - 10, b"/etc/motd\0").unwrap()), 3);
        // without its NUL it runs into the unmapped page
        assert_eq!(open_at(end - 9, &|space| space.write_bytes(end - 9, b"/etc/motd").unwrap()), errno(Errno::EFAULT));
        // no NUL within PATH_MAX
        let long = 0x70_0000;
        let rw = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::WRITABLE;
        let fill = |space: &mut UserSpace| {
            space.map(VirtAddr::new(long), 2 * 4096, rw).unwrap();
            space.write_bytes(long, &[b'a'; 2 * 4096]).unwrap();
        };
        assert_eq!(open_at(long, &fill), errno(Errno::ENAMETOOLONG));
    }

    #[test_case]
    fn test_dirfd() {
        let call = code!(osel_any_call, osel_any_call_end);
        let with_dirfd = |dirfd: u64, path: &[u8]| {
            let (_frame, stop, _loaded) = run_program(call, path, |frame| (frame.rax, frame.rdi, frame.rsi, frame.rdx) = (OPENAT, dirfd, DATA, 0));
            stop.exit_code()
        };
        assert_eq!(with_dirfd(99, b"/etc/motd"), 3, "an absolute path ignores dirfd");
        assert_eq!(with_dirfd(99, b"etc/motd"), errno(Errno::EBADF));
        assert_eq!(with_dirfd(1, b"etc/motd"), errno(Errno::ENOTDIR), "the terminal is not a directory");
        assert_eq!(with_dirfd(0x1234_5678_ffff_ff9c, b"etc/motd"), 3, "garbage above AT_FDCWD");
        assert_eq!(with_dirfd(0xffff_ffff_ffff_ff9c, b"etc/motd"), 3, "AT_FDCWD sign extended");
    }

    #[test_case]
    fn test_read_errors() {
        let open_read = |path: &[u8], flags: u64, count: u64| {
            let (_frame, stop, _loaded) = run_program(code!(osel_open_read, osel_open_read_end), path, |frame| {
                (frame.rdi, frame.rsi, frame.rdx, frame.r12) = (GO_AT_FDCWD, DATA, flags, count);
            });
            stop.exit_code()
        };
        assert_eq!(open_read(b"/etc", O_DIRECTORY as u64, 16), errno(Errno::EISDIR));
        assert_eq!(open_read(b"/dev/null", O_RDWR as u64, 16), 0, "nothing to read");
        assert_eq!(open_read(b"/dev/null", O_WRONLY as u64, 16), errno(Errno::EBADF), "write only");
        assert_eq!(open_read(b"/etc/motd", 0, 0), 0);
        let call = code!(osel_any_call, osel_any_call_end);
        let read = |fd: u64, buf: u64| {
            let (_frame, stop, _loaded) = run_program(call, b"", |frame| (frame.rax, frame.rdi, frame.rsi, frame.rdx) = (READ, fd, buf, 4));
            stop.exit_code()
        };
        assert_eq!(read(99, BUFFER), errno(Errno::EBADF));
        assert_eq!(read(0, 0), errno(Errno::EFAULT), "a bad buffer fails before the terminal is read");
    }

    #[test_case]
    fn test_close_and_fcntl() {
        let (_frame, stop, _loaded) = run_program(code!(osel_close_twice, osel_close_twice_end), b"", |_| {});
        assert_eq!(stop.exit_code(), errno(Errno::EBADF));
        let call = code!(osel_any_call, osel_any_call_end);
        let fcntl = |fd: u64, cmd: u64, arg: u64| {
            let (_frame, stop, _loaded) = run_program(call, b"", |frame| (frame.rax, frame.rdi, frame.rsi, frame.rdx) = (FCNTL, fd, cmd, arg));
            stop.exit_code()
        };
        // Go's checkfds: F_GETFD on 0, 1 and 2 must work
        for fd in 0..3 {
            assert_eq!(fcntl(fd, F_GETFD as u64, 0), 0, "fd {}", fd);
        }
        assert_eq!(fcntl(1, F_GETFL as u64, 0), O_RDWR as u8, "the low byte of O_RDWR | O_LARGEFILE");
        assert_eq!(fcntl(5, 9999, 0), errno(Errno::EBADF), "the descriptor first");
        assert_eq!(fcntl(1, F_DUPFD as u64, 10), 10);
        assert_eq!(fcntl(1, F_DUPFD_CLOEXEC as u64, 0), 3);
        assert_eq!(fcntl(5, F_GETFD as u64, 0), errno(Errno::EBADF));
        assert_eq!(fcntl(1, 9999, 0), errno(Errno::EINVAL));
    }

    /// With O_NONBLOCK an empty terminal answers EAGAIN instead of waiting: Go reads it
    /// this way once the netpoller has it
    #[test_case]
    fn test_a_nonblocking_terminal_does_not_wait() {
        let call = code!(osel_any_call, osel_any_call_end);
        while crate::keyboard::pop_char().is_some() {}
        let mut loaded = load(call);
        let mut files = crate::user::standard_files();
        files.get(0).unwrap().set_status(osel_core::file::O_NONBLOCK);
        let mut frame = initial_frame(loaded.entry, loaded.rsp);
        (frame.rax, frame.rdi, frame.rsi, frame.rdx) = (READ, 0, BUFFER, 16);
        let stop = crate::user::run_with(&mut loaded.space, &mut files, &mut frame, &mut osel_core::fpu::FpuState::initial());
        assert_eq!(stop.exit_code(), errno(Errno::EAGAIN));
    }

    #[test_case]
    fn test_a_typed_key_is_read_from_the_terminal() {
        // 'a' pressed and released
        crate::keyboard::handle_scancode(0x1e);
        crate::keyboard::handle_scancode(0x9e);
        let call = code!(osel_any_call, osel_any_call_end);
        let (_frame, stop, mut loaded) = run_program(call, b"", |frame| (frame.rax, frame.rdi, frame.rsi, frame.rdx) = (READ, 0, BUFFER, 16));
        assert_eq!(stop.exit_code(), 1);
        let mut key = [0u8; 1];
        loaded.space.read_bytes(BUFFER, &mut key).unwrap();
        assert_eq!(&key, b"a");
    }

    /// Descriptors a program leaves open are closed with it
    #[test_case]
    fn test_open_files_go_with_the_program() {
        let frames = crate::memory::used_frames();
        let heap = crate::allocator::with_allocator(|allocator| allocator.in_use());
        for _ in 0..50 {
            let (_frame, stop, _loaded) = run_program(code!(osel_open_many, osel_open_many_end), b"/etc/motd", |frame| frame.r12 = 20);
            assert_eq!(stop.exit_code(), 0);
        }
        assert_eq!(crate::memory::used_frames(), frames);
        assert_eq!(crate::allocator::with_allocator(|allocator| allocator.in_use()), heap);
    }
}
