//! syscall module is the `syscall` entry and the system calls the kernel implements
//!
//! Numbers, arguments and the choice of the way back live in `osel_core::syscall`; the
//! list of calls is in docs/reference/syscalls.md.

use crate::gdt::{self, USER_CODE_SELECTOR, USER_DATA_SELECTOR};
use crate::memory::{PAGE_SIZE, USER_END};
use crate::scheduler;
use crate::user::Stop;
use alloc::string::String;
use core::arch::naked_asm;
use osel_core::errno::{Errno, KResult};
use crate::fs;
use crate::memory::UserSpace;
use alloc::sync::Arc;
use alloc::vec;
use osel_core::file::{FdTable, O_CLOEXEC, O_CREAT, OpenDescription, encode_stat, open_access};
use osel_core::syscall::{
    self, CLOCK_GETTIME, CLONE, CLOSE, EXIT, EXIT_GROUP, FCNTL, FSTAT, GETPID, GETTID, MADVISE, MMAP, MPROTECT, MUNMAP,
    NANOSLEEP, NEWFSTATAT, OPENAT, READ, Return, SCHED_YIELD, SYSCALL_VECTOR, WRITE, fd_arg,
};
use osel_core::mmap::MAP_ANONYMOUS;
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
        // the running thread's kernel stack: the frame lands at its top
        "mov rsp, [rip + {stack_top}]",
        // labels for the test of the order: stack first, then interrupts
        ".global osel_syscall_on_kernel_stack", "osel_syscall_on_kernel_stack:",
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
        stack_top = sym scheduler::CURRENT_STACK_TOP,
        user_data = const USER_DATA_SELECTOR,
        user_code = const USER_CODE_SELECTOR,
        vector = const SYSCALL_VECTOR,
        dispatch = sym dispatch,
        iret = const Return::Iret as u64,
    )
}

/// Runs the call the program asked for with interrupts on, puts the result into RAX of the
/// frame and says how to go back
extern "C" fn dispatch(frame: &mut TrapFrame) -> u64 {
    let [a0, a1, a2, a3, a4, a5] = syscall::args(frame);
    let result = match syscall::number(frame) {
        Some(READ) => read(fd_arg(a0), a1, a2),
        Some(WRITE) => write(fd_arg(a0), a1, a2),
        Some(OPENAT) => openat(fd_arg(a0), a1, a2 as u32),
        Some(CLOSE) => files().close(fd_arg(a0)).map(|()| 0),
        Some(FSTAT) => fstat(fd_arg(a0), a1),
        Some(NEWFSTATAT) => newfstatat(fd_arg(a0), a1, a2, a3 as u32),
        Some(FCNTL) => fcntl(fd_arg(a0), a1 as u32, a2),
        Some(MMAP) => mmap(a0, a1, a2, a3, fd_arg(a4), a5),
        Some(MUNMAP) => space().munmap(a0, a1).map(|()| 0),
        Some(MPROTECT) => space().mprotect(a0, a1, prot_arg(a2)).map(|()| 0),
        Some(MADVISE) => space().madvise(a0, a1, a2 as u32 as i32 as u64).map(|()| 0),
        Some(GETPID) => Ok(1),
        Some(SCHED_YIELD) => {
            scheduler::yield_now();
            Ok(0)
        }
        Some(CLONE) => clone(frame, a0, a1),
        Some(NANOSLEEP) => nanosleep(a0, a1),
        Some(CLOCK_GETTIME) => clock_gettime(a0, a1),
        Some(GETTID) => Ok(scheduler::current_id()),
        Some(EXIT) => scheduler::exit_thread(Stop::Exit(a0 as u8), Some(*frame)),
        Some(EXIT_GROUP) => scheduler::exit_current(Stop::Exit(a0 as u8), Some(*frame)),
        _ => Err(Errno::ENOSYS),
    };
    frame.rax = syscall::encode(result);
    // a killed program goes no further than here
    if scheduler::current_killed() {
        scheduler::exit_current(Stop::Killed, Some(*frame));
    }
    match syscall::return_path(frame, USER_CODE_SELECTOR, USER_DATA_SELECTOR, USER_END) {
        // the return address is outside the lower half: neither way back is safe
        Return::Leave => scheduler::exit_current(scheduler::fault_of(frame, 0), Some(*frame)),
        path => path as u64,
    }
}

/// The address space of the running process, locked for the statement. Must not be held
/// across anything that blocks, and no program address is dereferenced through it.
fn space() -> crate::sync::IrqSpinlockGuard<'static, UserSpace> {
    // SAFETY: only called from `dispatch`, on a thread of the process
    unsafe { scheduler::current_space() }
}

fn files() -> crate::sync::IrqSpinlockGuard<'static, FdTable> {
    // SAFETY: as `space`
    unsafe { scheduler::current_files() }
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

/// The `clone` flags Go's `newosproc` uses: a new thread of the same process. OSel shares
/// the whole process (space and descriptors), so only this exact set is accepted.
const CLONE_THREAD_FLAGS: u64 = 0x100 | 0x200 | 0x400 | 0x800 | 0x10000 | 0x40000;

/// `clone(flags, child_stack, ...)`: starts another thread of the running process. The
/// child returns 0 with its own stack, the parent gets the child's thread id. Only the
/// thread flavour Go uses; other flags are `EINVAL`.
fn clone(frame: &TrapFrame, flags: u64, child_stack: u64) -> KResult<u64> {
    if flags & 0xff != 0 {
        // the low byte is the signal sent on exit; Go passes 0
        return Err(Errno::EINVAL);
    }
    if flags & !0xff != CLONE_THREAD_FLAGS {
        return Err(Errno::EINVAL);
    }
    if child_stack == 0 || child_stack > USER_END {
        return Err(Errno::EINVAL);
    }
    let mut child = *frame;
    child.rax = 0;
    child.rsp = child_stack;
    let fs_base = x86_64::registers::model_specific::FsBase::read().as_u64();
    let mut fpu = osel_core::fpu::FpuState::initial();
    crate::fpu::save(&mut fpu);
    // SAFETY: called on a thread of the process, which stays alive across the call
    let process = unsafe { scheduler::current_process_arc() };
    scheduler::spawn_in(process, child, fpu, fs_base)
}

/// `nanosleep(req, rem)`: sleeps for the `timespec` at `req`. On a kill it reports the
/// time left at `rem` (if given) and `EINTR`.
fn nanosleep(req: u64, rem: u64) -> KResult<u64> {
    let mut bytes = [0u8; 16];
    space().copy_from_user(req, &mut bytes)?;
    let duration = osel_core::clock::decode_timespec(bytes).ok_or(Errno::EINVAL)?;
    let deadline = crate::time::monotonic_ns().saturating_add(duration);
    match crate::time::sleep_until(deadline) {
        Ok(()) => Ok(0),
        Err(error) => {
            if rem != 0 {
                let now = crate::time::monotonic_ns();
                let left = deadline.saturating_sub(now);
                space().copy_to_user(rem, &osel_core::clock::encode_timespec(left))?;
            }
            Err(error)
        }
    }
}

/// `clock_gettime(clock_id, timespec)`
fn clock_gettime(clock_id: u64, timespec: u64) -> KResult<u64> {
    let now = crate::time::clock_ns(clock_id).ok_or(Errno::EINVAL)?;
    space().copy_to_user(timespec, &osel_core::clock::encode_timespec(now))?;
    Ok(0)
}

/// `PROT_*` bits; bits beyond 32 make it `EINVAL` like unknown ones
fn prot_arg(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// `mmap(addr, len, prot, flags, fd, offset)`: anonymous memory only, a file is `ENODEV`
fn mmap(addr: u64, len: u64, prot: u64, flags: u64, fd: i32, offset: u64) -> KResult<u64> {
    if offset % PAGE_SIZE != 0 {
        return Err(Errno::EINVAL);
    }
    let flags = flags as u32 as u64;
    if flags & MAP_ANONYMOUS == 0 {
        files().get(fd)?;
    }
    space().mmap(addr, len, prot_arg(prot), flags)
}

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
    let mut files = files();
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

/// `mmap`, `munmap`, `mprotect`, `madvise` and pages mapped on the first touch
#[cfg(test)]
mod memory_tests {
    use super::*;
    use crate::memory::used_frames;
    use crate::program::Loaded;
    use crate::user::tests::{code, load};
    use crate::user::{Stop, initial_frame, run};
    use core::arch::global_asm;
    use osel_core::mmap::{MAP_FIXED, MAP_PRIVATE, MMAP_TOP};
    use osel_core::trap::{INVALID_OPCODE, PAGE_FAULT, PF_FETCH, PF_PRESENT, PF_WRITE};
    use osel_core::user_mem::Access;
    use osel_core::vma::{PROT_EXEC, PROT_READ, PROT_WRITE};

    const P: u64 = PAGE_SIZE;
    const MIB: u64 = 1 << 20;
    const RW: u32 = PROT_READ | PROT_WRITE;
    const ANON: u64 = MAP_PRIVATE | MAP_ANONYMOUS;

    global_asm!(
        ".pushsection .rodata.osel_memory_programs, \"a\"",
        // mmap with the registers the test set, write 7 to the first page, read the second
        // into rcx, stop with rbx = the address
        ".global osel_mmap_touch", "osel_mmap_touch:",
        "mov eax, 9",
        "syscall",
        "mov rbx, rax",
        "mov byte ptr [rax], 7",
        "movzx ecx, byte ptr [rax + 4096]",
        "ud2",
        ".global osel_mmap_touch_end", "osel_mmap_touch_end:",
        // write 1 to [rdi]
        ".global osel_store", "osel_store:",
        "mov byte ptr [rdi], 1",
        "ud2",
        ".global osel_store_end", "osel_store_end:",
        // read [rdi] into rcx
        ".global osel_load", "osel_load:",
        "movzx ecx, byte ptr [rdi]",
        "ud2",
        ".global osel_load_end", "osel_load_end:",
        // jump to rdi
        ".global osel_jump", "osel_jump:",
        "jmp rdi",
        ".global osel_jump_end", "osel_jump_end:",
        // read the read-only page at rdi, make it RW, write 9 and read it back into rcx;
        // rbx = what mprotect returned
        ".global osel_widen", "osel_widen:",
        "mov r12, rdi",
        "movzx ebx, byte ptr [rdi]",
        "mov esi, 4096",
        "mov edx, 3",
        "mov eax, 10",
        "syscall",
        "mov rbx, rax",
        "mov byte ptr [r12], 9",
        "movzx ecx, byte ptr [r12]",
        "ud2",
        ".global osel_widen_end", "osel_widen_end:",
        // write 5 to [rdi], MADV_DONTNEED the page, read it into rcx; rbx = what madvise returned
        ".global osel_dontneed", "osel_dontneed:",
        "mov r12, rdi",
        "mov byte ptr [rdi], 5",
        "mov esi, 4096",
        "mov edx, 4",
        "mov eax, 28",
        "syscall",
        "mov rbx, rax",
        "movzx ecx, byte ptr [r12]",
        "ud2",
        ".global osel_dontneed_end", "osel_dontneed_end:",
        // any call with the registers the test set, exit with the result
        ".global osel_memory_call", "osel_memory_call:",
        "syscall",
        "mov rdi, rax",
        "mov eax, 60",
        "syscall",
        ".global osel_memory_call_end", "osel_memory_call_end:",
        ".popsection",
    );

    /// Runs `code` on `loaded` with registers the test sets
    fn run_on(loaded: &mut Loaded, setup: impl FnOnce(&mut TrapFrame)) -> (TrapFrame, Stop) {
        let mut frame = initial_frame(loaded.entry, loaded.rsp);
        setup(&mut frame);
        let stop = run(&mut loaded.space, &mut frame);
        (frame, stop)
    }

    fn byte_at(loaded: &mut Loaded, address: u64) -> u8 {
        let mut byte = [0u8];
        loaded.space.read_bytes(address, &mut byte).unwrap();
        byte[0]
    }

    fn errno(errno: Errno) -> u8 {
        (-(errno.code())) as u8
    }

    /// The program's own `mmap` gives zeroed memory, and only touched pages take frames
    #[test_case]
    fn test_mmap_memory_is_lazy_and_zero() {
        let mut loaded = load(code!(osel_mmap_touch, osel_mmap_touch_end));
        let before = used_frames();
        let (frame, stop) = run_on(&mut loaded, |frame| {
            (frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8, frame.r9) = (0, 64 * MIB, RW as u64, ANON, u64::MAX, 0);
        });
        assert_eq!(stop.fault().vector, INVALID_OPCODE);
        assert_eq!(frame.rbx, MMAP_TOP - 64 * MIB, "from the top");
        assert_eq!(frame.rcx, 0, "fresh memory reads zero");
        assert_eq!(byte_at(&mut loaded, frame.rbx), 7);
        let used = used_frames() - before;
        assert!((2..=5).contains(&used), "two pages and their tables, not 64 MiB: {used}");
    }

    /// Go's start: a gigabyte of `PROT_NONE` reservations, then memory committed inside
    /// with `MAP_FIXED`
    #[test_case]
    fn test_reservations_cost_nothing() {
        let mut loaded = load(code!(osel_store, osel_store_end));
        let before = used_frames();
        let hint = 0xc0_0000_0000;
        assert_eq!(loaded.space.mmap(hint, 64 * MIB, 0, ANON), Ok(hint));
        loaded.space.mmap(0, 512 * MIB, 0, ANON).unwrap();
        loaded.space.mmap(0, 512 * MIB, 0, ANON).unwrap();
        assert_eq!(used_frames(), before, "reservations take no frames");
        assert_eq!(loaded.space.mmap(hint + 4 * MIB, 4 * MIB, RW, ANON | MAP_FIXED), Ok(hint + 4 * MIB));
        let (_frame, stop) = run_on(&mut loaded, |frame| frame.rdi = hint + 5 * MIB);
        assert_eq!(stop.fault().vector, INVALID_OPCODE);
        assert_eq!(byte_at(&mut loaded, hint + 5 * MIB), 1);
        let (_frame, stop) = run_on(&mut loaded, |frame| frame.rdi = hint + 8 * MIB);
        let trap = stop.fault();
        assert_eq!((trap.vector, trap.cr2), (PAGE_FAULT, hint + 8 * MIB), "the reservation around stays PROT_NONE");
    }

    #[test_case]
    fn test_rights_of_an_area() {
        let mut loaded = load(code!(osel_load, osel_load_end));
        let none = loaded.space.mmap(0, P, 0, ANON).unwrap();
        let (_frame, stop) = run_on(&mut loaded, |frame| frame.rdi = none);
        let trap = stop.fault();
        assert_eq!((trap.vector, trap.cr2, trap.error_code & PF_PRESENT), (PAGE_FAULT, none, 0), "PROT_NONE can't be read");

        let mut loaded = load(code!(osel_store, osel_store_end));
        let read = loaded.space.mmap(0, P, PROT_READ, ANON).unwrap();
        let (_frame, stop) = run_on(&mut loaded, |frame| frame.rdi = read);
        let trap = stop.fault();
        assert_eq!((trap.vector, trap.cr2), (PAGE_FAULT, read), "a read-only page can't be written");
        assert_eq!(trap.error_code & (PF_PRESENT | PF_WRITE), PF_WRITE, "a write to an untouched page is not allowed either");

        let mut loaded = load(code!(osel_jump, osel_jump_end));
        let data = loaded.space.mmap(0, P, RW, ANON).unwrap();
        let (_frame, stop) = run_on(&mut loaded, |frame| frame.rdi = data);
        let trap = stop.fault();
        assert_eq!((trap.vector, trap.cr2), (PAGE_FAULT, data));
        assert_ne!(trap.error_code & PF_FETCH, 0, "data is not code");
    }

    /// Jumping into an untouched `PROT_EXEC` page maps it on the fetch: its zeros decode as
    /// `add [rax], al`, which faults at `rax`, a `PROT_NONE` page
    #[test_case]
    fn test_a_fetch_maps_a_lazy_page() {
        let mut loaded = load(code!(osel_jump, osel_jump_end));
        let code = loaded.space.mmap(0, P, PROT_READ | PROT_EXEC, ANON).unwrap();
        let none = loaded.space.mmap(0, P, 0, ANON).unwrap();
        let (frame, stop) = run_on(&mut loaded, |frame| (frame.rdi, frame.rax) = (code, none));
        let trap = stop.fault();
        assert_eq!((trap.vector, trap.cr2, frame.rip), (PAGE_FAULT, none, code), "the code page was mapped and ran");
    }

    /// A page of a `PROT_EXEC` area runs after its first touch, here the kernel's
    #[test_case]
    fn test_lazy_code_runs() {
        let mut loaded = load(code!(osel_jump, osel_jump_end));
        let code = loaded.space.mmap(0, P, PROT_READ | PROT_EXEC, ANON).unwrap();
        loaded.space.check_user(code, 2, Access::Read).unwrap();
        loaded.space.write_bytes(code, &[0x0f, 0x0b]).unwrap();
        let (frame, stop) = run_on(&mut loaded, |frame| frame.rdi = code);
        assert_eq!(stop.fault().vector, INVALID_OPCODE);
        assert_eq!(frame.rip, code);
    }

    /// A page first touched under `PROT_READ` gets its page tables; they must not keep the
    /// write right from it once `mprotect` allows writing
    #[test_case]
    fn test_mprotect_widens_rights() {
        let mut loaded = load(code!(osel_widen, osel_widen_end));
        // a fresh 1 GiB slot: its tables are made by the read
        let addr = loaded.space.mmap(0x7000_0000_0000, P, PROT_READ, ANON).unwrap();
        let (frame, stop) = run_on(&mut loaded, |frame| frame.rdi = addr);
        assert_eq!(stop.fault().vector, INVALID_OPCODE);
        assert_eq!((frame.rbx, frame.rcx), (0, 9));
    }

    #[test_case]
    fn test_mprotect_none_keeps_the_data() {
        let mut loaded = load(code!(osel_load, osel_load_end));
        let addr = loaded.space.mmap(0, 2 * P, RW, ANON).unwrap();
        loaded.space.check_user(addr, 1, Access::Write).unwrap();
        loaded.space.write_bytes(addr, &[42]).unwrap();
        let frames = used_frames();
        loaded.space.mprotect(addr, 2 * P, 0).unwrap();
        assert_eq!(used_frames(), frames, "the frame stays");
        assert_eq!(loaded.space.check_user(addr, 1, Access::Read), Err(Errno::EFAULT), "the kernel can't use it for the program");
        let (_frame, stop) = run_on(&mut loaded, |frame| frame.rdi = addr);
        assert_eq!(stop.fault().vector, PAGE_FAULT);
        loaded.space.mprotect(addr, P, PROT_READ).unwrap();
        let (frame, stop) = run_on(&mut loaded, |frame| frame.rdi = addr);
        assert_eq!(stop.fault().vector, INVALID_OPCODE);
        assert_eq!(frame.rcx, 42, "the data came back");
        loaded.space.mprotect(addr, P, 0).unwrap();
        drop(loaded);
        // the frame of a PROT_NONE page goes with the program
    }

    #[test_case]
    fn test_madvise_dontneed_gives_zeros() {
        let mut loaded = load(code!(osel_dontneed, osel_dontneed_end));
        let addr = loaded.space.mmap(0, P, RW, ANON).unwrap();
        let (frame, stop) = run_on(&mut loaded, |frame| frame.rdi = addr);
        assert_eq!(stop.fault().vector, INVALID_OPCODE);
        assert_eq!((frame.rbx, frame.rcx), (0, 0));
    }

    #[test_case]
    fn test_munmap_takes_the_pages() {
        let mut loaded = load(code!(osel_load, osel_load_end));
        let addr = loaded.space.mmap(0, 3 * P, RW, ANON).unwrap();
        loaded.space.check_user(addr, 3 * P as usize, Access::Write).unwrap();
        let frames = used_frames();
        loaded.space.munmap(addr + P, P).unwrap();
        assert_eq!(used_frames(), frames - 1);
        let (_frame, stop) = run_on(&mut loaded, |frame| frame.rdi = addr + P);
        assert_eq!(stop.fault().cr2, addr + P);
        let (_frame, stop) = run_on(&mut loaded, |frame| frame.rdi = addr + 2 * P);
        assert_eq!(stop.fault().vector, INVALID_OPCODE, "the rest of the area stays");
        // a huge range walks only the tables there are
        loaded.space.munmap(0x10_0000_0000, 1 << 46).unwrap();
    }

    /// The kernel writes into an untouched page of an area like the program would
    #[test_case]
    fn test_kernel_copies_reach_lazy_pages() {
        let call = code!(osel_memory_call, osel_memory_call_end);
        let mut loaded = load(call);
        let stat = loaded.space.mmap(0, P, RW, ANON).unwrap();
        let none = loaded.space.mmap(0, P, 0, ANON).unwrap();
        let read = loaded.space.mmap(0, P, PROT_READ, ANON).unwrap();
        for (buffer, expected) in [(stat, 0), (none, errno(Errno::EFAULT)), (read, errno(Errno::EFAULT))] {
            let (_frame, stop) = run_on(&mut loaded, |frame| (frame.rax, frame.rdi, frame.rsi) = (FSTAT, 1, buffer));
            assert_eq!(stop.exit_code(), expected, "{buffer:#x}");
        }
        assert_ne!(loaded.space.page_flags(VirtAddr::new(stat)), None);
        // and reads from one: the path there is empty
        let (_frame, stop) =
            run_on(&mut loaded, |frame| (frame.rax, frame.rdi, frame.rsi, frame.rdx) = (NEWFSTATAT, AT_FDCWD as u32 as u64, read, stat));
        assert_eq!(stop.exit_code(), errno(Errno::ENOENT));
    }

    #[test_case]
    fn test_memory_calls_through_syscall() {
        let call = code!(osel_memory_call, osel_memory_call_end);
        let mut loaded = load(call);
        let call_on = |loaded: &mut Loaded, rax: u64, args: [u64; 6]| {
            let (_frame, stop) = run_on(loaded, |frame| {
                (frame.rax, frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8, frame.r9) =
                    (rax, args[0], args[1], args[2], args[3], args[4], args[5]);
            });
            stop.exit_code()
        };
        assert_eq!(call_on(&mut loaded, MMAP, [0, 0, 3, ANON, u64::MAX, 0]), errno(Errno::EINVAL), "length 0");
        assert_eq!(call_on(&mut loaded, MMAP, [0, P, 3, ANON, u64::MAX, 1]), errno(Errno::EINVAL), "unaligned offset");
        assert_eq!(call_on(&mut loaded, MMAP, [0, P, 3, MAP_PRIVATE, u64::MAX, 0]), errno(Errno::EBADF), "a file without a descriptor");
        assert_eq!(call_on(&mut loaded, MMAP, [0, P, 3, MAP_PRIVATE, 1, 0]), errno(Errno::ENODEV), "a file");
        assert_eq!(call_on(&mut loaded, MMAP, [P, P, 3, ANON | MAP_FIXED, u64::MAX, 0]), errno(Errno::EPERM), "below 0x10000");
        assert_eq!(call_on(&mut loaded, MMAP, [0, P, 1 << 32 | 3, ANON, u64::MAX, 0]), errno(Errno::EINVAL), "rights beyond 32 bits");
        let addr = 0x50_0000_0000;
        assert_eq!(call_on(&mut loaded, MMAP, [addr, P, 3, ANON | MAP_FIXED, u64::MAX, 0]), addr as u8);
        assert!(loaded.space.mappings().find(addr).is_some());
        assert_eq!(call_on(&mut loaded, MPROTECT, [addr, P, 1, 0, 0, 0]), 0);
        assert_eq!(call_on(&mut loaded, MPROTECT, [addr, 2 * P, 1, 0, 0, 0]), errno(Errno::ENOMEM), "a hole");
        assert_eq!(call_on(&mut loaded, MADVISE, [addr, P, 15, 0, 0, 0]), 0, "MADV_NOHUGEPAGE");
        assert_eq!(call_on(&mut loaded, MADVISE, [addr, P, 0xffff_ffff_0000_0004, 0, 0, 0]), 0, "advice is an int");
        assert_eq!(call_on(&mut loaded, MADVISE, [addr, P, 77, 0, 0, 0]), errno(Errno::EINVAL));
        assert_eq!(call_on(&mut loaded, MUNMAP, [addr, P, 0, 0, 0, 0]), 0);
        assert_eq!(call_on(&mut loaded, MUNMAP, [addr + 1, P, 0, 0, 0, 0]), errno(Errno::EINVAL));
        assert!(loaded.space.mappings().find(addr).is_none());
    }

    /// `MAP_FIXED` over touched pages and `munmap` of the program's own pages give the
    /// frames back
    #[test_case]
    fn test_replaced_and_unmapped_pages_are_freed() {
        let mut loaded = load(code!(osel_load, osel_load_end));
        let addr = loaded.space.mmap(0, 4 * P, RW, ANON).unwrap();
        loaded.space.check_user(addr, 4 * P as usize, Access::Write).unwrap();
        let frames = used_frames();
        loaded.space.mmap(addr, 4 * P, RW, ANON | MAP_FIXED).unwrap();
        // the level 3 table stays: the stack is in it too
        assert_eq!(used_frames(), frames - 4 - 2, "the pages and their emptied level 1 and 2 tables");
        let stack_top = crate::program::STACK_TOP;
        let frames = used_frames();
        loaded.space.munmap(stack_top - 4 * P, 4 * P).unwrap();
        assert_eq!(used_frames(), frames - 4, "eagerly mapped stack pages");
        let (_frame, stop) = run_on(&mut loaded, |frame| frame.rdi = stack_top - P);
        assert_eq!(stop.fault().cr2, stack_top - P);
    }

    /// The program's own segments and stack are areas too
    #[test_case]
    fn test_program_pages_are_areas() {
        let loaded = load(code!(osel_load, osel_load_end));
        let text = loaded.space.mappings().find(loaded.entry).expect("the code is an area");
        assert_eq!(text.prot, PROT_READ | PROT_EXEC);
        let stack = loaded.space.mappings().find(loaded.rsp).expect("the stack is an area");
        assert_eq!(stack.prot, RW);
        assert!(loaded.space.mappings().find(crate::program::STACK_BOTTOM - 8).is_none(), "the guard page is not");
    }

    #[test_case]
    fn test_memory_programs_leak_nothing() {
        let frames = used_frames();
        let heap = crate::allocator::with_allocator(|allocator| allocator.in_use());
        for _ in 0..30 {
            let mut loaded = load(code!(osel_mmap_touch, osel_mmap_touch_end));
            let none = loaded.space.mmap(0, 2 * P, RW, ANON).unwrap();
            loaded.space.check_user(none, 2 * P as usize, Access::Write).unwrap();
            loaded.space.mprotect(none, 2 * P, 0).unwrap();
            let (_frame, stop) = run_on(&mut loaded, |frame| {
                (frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8, frame.r9) = (0, 64 * MIB, RW as u64, ANON, u64::MAX, 0);
            });
            assert_eq!(stop.fault().vector, INVALID_OPCODE);
        }
        assert_eq!(used_frames(), frames);
        assert_eq!(crate::allocator::with_allocator(|allocator| allocator.in_use()), heap);
    }
}

/// `clock_gettime` and `nanosleep`
#[cfg(test)]
mod time_tests {
    use super::*;
    use crate::program::Loaded;
    use crate::user::tests::{code, load};
    use crate::user::{Stop, initial_frame, run};
    use core::arch::global_asm;
    use osel_core::clock::{self, NANOS_PER_SEC};

    const DATA: u64 = 0x60_0000;

    global_asm!(
        ".pushsection .rodata.osel_time_programs, \"a\"",
        // clock_gettime(rdi, DATA) twice with a busy wait between, exit 0; the test reads
        // both timespecs from DATA (first) and DATA+16 (second)
        ".global osel_two_clocks", "osel_two_clocks:",
        "mov esi, 0x600000",
        "mov eax, 228",
        "syscall",
        "test rax, rax",
        "js 3f",
        "mov r12d, 0x4000000",
        "2:", "dec r12d", "jnz 2b",
        "mov edi, 1",
        "mov esi, 0x600010",
        "mov eax, 228",
        "syscall",
        "3:",
        "mov rdi, rax",
        "mov eax, 60",
        "syscall",
        ".global osel_two_clocks_end", "osel_two_clocks_end:",
        // nanosleep(DATA, 0), exit with the result
        ".global osel_sleep", "osel_sleep:",
        "mov edi, 0x600000",
        "xor esi, esi",
        "mov eax, 35",
        "syscall",
        "mov rdi, rax",
        "mov eax, 60",
        "syscall",
        ".global osel_sleep_end", "osel_sleep_end:",
        ".popsection",
    );

    fn run_on(loaded: &mut Loaded, setup: impl FnOnce(&mut TrapFrame)) -> Stop {
        let mut frame = initial_frame(loaded.entry, loaded.rsp);
        setup(&mut frame);
        run(&mut loaded.space, &mut frame)
    }

    fn read_nanos(loaded: &mut Loaded, at: u64) -> u64 {
        let mut bytes = [0u8; 16];
        loaded.space.read_bytes(at, &mut bytes).unwrap();
        clock::decode_timespec(bytes).unwrap()
    }

    /// `clock_gettime` gives a monotonic time that does not go backwards
    #[test_case]
    fn test_clock_gettime_moves_forward() {
        let mut loaded = load(code!(osel_two_clocks, osel_two_clocks_end));
        assert_eq!(run_on(&mut loaded, |frame| frame.rdi = 1).exit_code(), 0);
        let (first, second) = (read_nanos(&mut loaded, DATA), read_nanos(&mut loaded, DATA + 16));
        assert!(second >= first, "monotonic time went backwards: {first} then {second}");
        assert!(second < first + 10 * NANOS_PER_SEC, "the busy wait did not take ten seconds");
    }

    /// `CLOCK_REALTIME` is a plausible wall-clock time, well past 2020
    #[test_case]
    fn test_realtime_is_a_real_date() {
        let mut loaded = load(code!(osel_two_clocks, osel_two_clocks_end));
        assert_eq!(run_on(&mut loaded, |frame| frame.rdi = 0).exit_code(), 0);
        let realtime = read_nanos(&mut loaded, DATA) / NANOS_PER_SEC;
        assert!(realtime > 1_577_836_800, "CLOCK_REALTIME is before 2020: {realtime}");
    }

    /// An unknown clock is `EINVAL`
    #[test_case]
    fn test_unknown_clock_is_einval() {
        let mut loaded = load(code!(osel_two_clocks, osel_two_clocks_end));
        let code = run_on(&mut loaded, |frame| frame.rdi = 99).exit_code();
        assert_eq!(code, (-(Errno::EINVAL.code())) as u8);
    }

    /// `nanosleep` blocks until the deadline: the timer advances by at least the requested
    /// ticks while the thread sleeps, and other threads run meanwhile
    #[test_case]
    fn test_nanosleep_waits_for_the_clock() {
        let mut loaded = load(code!(osel_sleep, osel_sleep_end));
        // sleep 50 ms (5 ticks at 100 Hz)
        loaded.space.check_user(DATA, 16, MemAccess::Write).unwrap();
        loaded.space.write_bytes(DATA, &clock::encode_timespec(50_000_000)).unwrap();
        let before = crate::interrupts::ticks();
        assert_eq!(run_on(&mut loaded, |_| {}).exit_code(), 0);
        let elapsed = crate::interrupts::ticks() - before;
        assert!(elapsed >= 5, "slept fewer than 5 ticks: {elapsed}");
    }
}
