#![no_std]
#![no_main]

//! Raw Linux ABI checks with no Go runtime hiding the syscall results.

extern crate alloc;

use bootloader_api::{BootInfo, entry_point};
use core::arch::global_asm;
use core::panic::PanicInfo;
use osel_core::elf::{ElfBuilder, PF_R, PF_W, PF_X};

entry_point!(main, config = &osel::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    osel::init(boot_info);
    osel::testosterone::test_runner(&[
        &bad_pipe_pointer_and_exact_dup,
        &nonblocking_small_write_is_atomic,
        &failed_exec_preserves_image_and_cloexec,
        &exec_wait_status_and_nohang,
        &exiting_parent_reclaims_blocked_child,
        &kill_interrupts_untimed_futex,
        &kill_interrupts_long_sleep,
        &child_limit_recovers_after_reaping,
    ]);
    osel::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    osel::testosterone::test_panic_handler(info)
}

macro_rules! code {
    ($start:ident, $end:ident) => {{
        unsafe extern "C" { static $start: u8; static $end: u8; }
        let start = core::ptr::addr_of!($start);
        let end = core::ptr::addr_of!($end);
        unsafe { core::slice::from_raw_parts(start, end as usize - start as usize) }
    }};
}

fn load(code: &[u8]) -> osel::program::Loaded {
    let image = ElfBuilder::new(0x400000)
        .segment(0x400000, PF_R | PF_X, code, code.len() as u64)
        .segment(0x600000, PF_R | PF_W, &[], 4096)
        .build();
    osel::program::load_file(&image[..], &[], &[]).unwrap()
}

fn run(code: &[u8]) {
    let mut loaded = load(code);
    let mut frame = osel::user::initial_frame(loaded.entry, loaded.rsp);
    let stop = osel::user::run(&mut loaded.space, &mut frame);
    assert_eq!(stop.exit_code(), 0, "failed at assertion {}", frame.r12);
}

fn bad_pipe_pointer_and_exact_dup() { run(code!(fd_checks, fd_checks_end)); }
fn nonblocking_small_write_is_atomic() { run(code!(atomic_checks, atomic_checks_end)); }
fn failed_exec_preserves_image_and_cloexec() { run(code!(bad_exec_checks, bad_exec_checks_end)); }
fn exec_wait_status_and_nohang() { run(code!(wait_checks, wait_checks_end)); }
fn exiting_parent_reclaims_blocked_child() { run(code!(orphan_checks, orphan_checks_end)); }

fn kill_waiter(code: &[u8]) {
    let mut loaded = load(code);
    let frame = osel::user::initial_frame(loaded.entry, loaded.rsp);
    let process = osel::user::start(
        loaded.space.take(), osel::user::standard_files(), frame,
        osel_core::fpu::FpuState::initial(), 0,
    ).unwrap();
    // The new thread runs until it parks; a kill must return its blocked syscall so
    // the descriptor and buffer locals can be dropped normally.
    osel::scheduler::yield_now();
    process.kill();
    assert!(matches!(process.wait().stop, osel::user::Stop::Killed));
}

fn kill_interrupts_untimed_futex() { kill_waiter(code!(kill_futex, kill_futex_end)); }
fn kill_interrupts_long_sleep() { kill_waiter(code!(kill_sleep, kill_sleep_end)); }
fn child_limit_recovers_after_reaping() { run(code!(limit_checks, limit_checks_end)); }

global_asm!(
    ".section .text",
    ".global fd_checks, fd_checks_end",
    "fd_checks:",
    "mov r12d, 1",
    "mov eax, 293", "xor edi, edi", "xor esi, esi", "syscall",
    "cmp rax, -14", "jne 9f",
    "inc r12d",
    "mov eax, 293", "mov edi, 0x600000", "mov esi, 0x80000", "syscall",
    "test rax, rax", "jne 9f",
    "cmp dword ptr [0x600000], 3", "jne 9f",
    "cmp dword ptr [0x600004], 4", "jne 9f",
    "inc r12d",
    "mov eax, 33", "mov edi, 3", "mov esi, 3", "syscall",
    "cmp rax, 3", "jne 9f",
    "inc r12d",
    "mov eax, 72", "mov edi, 3", "mov esi, 1", "xor edx, edx", "syscall",
    "cmp rax, 1", "jne 9f", // dup2(fd,fd) preserved CLOEXEC
    "inc r12d",
    "mov eax, 292", "mov edi, 3", "mov esi, 3", "xor edx, edx", "syscall",
    "cmp rax, -22", "jne 9f",
    "inc r12d",
    "mov eax, 33", "mov edi, -1", "mov esi, 4", "syscall",
    "cmp rax, -9", "jne 9f",
    "inc r12d",
    "mov eax, 33", "mov edi, 3", "mov esi, 7", "syscall",
    "cmp rax, 7", "jne 9f",
    "inc r12d",
    "mov eax, 72", "mov edi, 7", "mov esi, 1", "xor edx, edx", "syscall",
    "test rax, rax", "jne 9f", // dup2 clears destination CLOEXEC
    "inc r12d",
    "mov byte ptr [0x600010], 42",
    "mov eax, 1", "mov edi, 4", "mov esi, 0x600010", "mov edx, 1", "syscall",
    "cmp rax, 1", "jne 9f",
    "inc r12d",
    "xor eax, eax", "mov edi, 7", "mov esi, 0x600011", "mov edx, 1", "syscall",
    "cmp rax, 1", "jne 9f", "cmp byte ptr [0x600011], 42", "jne 9f",
    "xor edi, edi", "jmp 8f",
    "9:", "mov edi, r12d",
    "8:", "mov eax, 60", "syscall", "ud2",
    "fd_checks_end:",

    ".global atomic_checks, atomic_checks_end",
    "atomic_checks:",
    "mov r12d, 1",
    "mov eax, 293", "mov edi, 0x600000", "mov esi, 0x800", "syscall",
    "test rax, rax", "jne 9f",
    "inc r12d", "mov ebx, 4",
    "2:",
    "mov eax, 1", "mov edi, 4", "mov esi, 0x600000", "mov edx, 4096", "syscall",
    "cmp rax, 4096", "jne 9f", "dec ebx", "jnz 2b",
    "inc r12d",
    "xor eax, eax", "mov edi, 3", "mov esi, 0x600000", "mov edx, 512", "syscall",
    "cmp rax, 512", "jne 9f",
    "inc r12d",
    // 1024 <= PIPE_BUF, but only 512 bytes fit: no prefix may be written.
    "mov eax, 1", "mov edi, 4", "mov esi, 0x600000", "mov edx, 1024", "syscall",
    "cmp rax, -11", "jne 9f",
    "inc r12d",
    "mov eax, 1", "mov edi, 4", "mov esi, 0x600000", "mov edx, 512", "syscall",
    "cmp rax, 512", "jne 9f",
    "inc r12d", "mov ebx, 4",
    "3:",
    "xor eax, eax", "mov edi, 3", "mov esi, 0x600000", "mov edx, 4096", "syscall",
    "cmp rax, 4096", "jne 9f", "dec ebx", "jnz 3b",
    "inc r12d",
    "xor eax, eax", "mov edi, 3", "mov esi, 0x600000", "mov edx, 1", "syscall",
    "cmp rax, -11", "jne 9f",
    "xor edi, edi", "jmp 8f",
    "9:", "mov edi, r12d",
    "8:", "mov eax, 60", "syscall", "ud2",
    "atomic_checks_end:",

    ".global bad_exec_checks, bad_exec_checks_end",
    "bad_exec_checks:",
    "mov r12d, 1",
    "mov eax, 293", "mov edi, 0x600000", "mov esi, 0x80000", "syscall",
    "test rax, rax", "jne 9f",
    "inc r12d",
    "mov eax, 59", "lea rdi, [rip + 7f]", "xor esi, esi", "xor edx, edx", "syscall",
    "cmp rax, -8", "jne 9f",
    "inc r12d",
    "mov eax, 72", "mov edi, 3", "mov esi, 1", "syscall",
    "cmp rax, 1", "jne 9f", // failing exec must not close error-report pipes
    "inc r12d",
    "mov eax, 59", "lea rdi, [rip + 6f]", "mov esi, 1", "xor edx, edx", "syscall",
    "cmp rax, -14", "jne 9f",
    "xor edi, edi", "jmp 8f",
    "9:", "mov edi, r12d",
    "8:", "mov eax, 60", "syscall", "ud2",
    "7:", ".asciz \"/bin/bad-exec\"",
    "6:", ".asciz \"/bin/clear\"",
    "bad_exec_checks_end:",

    ".global wait_checks, wait_checks_end",
    "wait_checks:",
    "mov r12d, 1",
    "mov eax, 293", "mov edi, 0x600000", "xor esi, esi", "syscall",
    "test rax, rax", "jne 9f",
    "inc r12d",
    "mov eax, 56", "mov edi, 0x4111", "xor esi, esi", "syscall",
    "test rax, rax", "js 9f", "jz 5f",
    "mov r13, rax",
    "inc r12d",
    "mov eax, 61", "mov rdi, r13", "mov esi, 0x600020", "mov edx, 1", "xor r10d, r10d", "syscall",
    "test rax, rax", "jne 9f", // child cannot exit until the parent writes
    "inc r12d",
    "mov eax, 61", "mov rdi, r13", "mov esi, 1", "xor edx, edx", "xor r10d, r10d", "syscall",
    "cmp rax, -14", "jne 9f", // an invalid status address must not consume the child
    "inc r12d",
    "mov eax, 1", "mov edi, 4", "mov esi, 0x600010", "mov edx, 1", "syscall",
    "cmp rax, 1", "jne 9f",
    "inc r12d",
    "mov eax, 61", "mov rdi, r13", "mov esi, 0x600020", "xor edx, edx", "xor r10d, r10d", "syscall",
    "cmp rax, r13", "jne 9f", "cmp dword ptr [0x600020], 0x2500", "jne 9f",
    "inc r12d",
    "mov eax, 61", "mov rdi, r13", "xor esi, esi", "xor edx, edx", "xor r10d, r10d", "syscall",
    "cmp rax, -10", "jne 9f",
    "xor edi, edi", "jmp 8f",
    "5:",
    "mov eax, 59", "lea rdi, [rip + 7f]", "xor esi, esi", "xor edx, edx", "syscall",
    "mov r12d, 99",
    "9:", "mov edi, r12d",
    "8:", "mov eax, 60", "syscall", "ud2",
    "7:", ".asciz \"/bin/pipe-child37\"",
    "wait_checks_end:",

    ".global orphan_checks, orphan_checks_end",
    "orphan_checks:",
    "mov r12d, 1",
    "mov eax, 293", "mov edi, 0x600000", "xor esi, esi", "syscall",
    "test rax, rax", "jne 9f",
    "inc r12d",
    "mov eax, 56", "mov edi, 0x4111", "xor esi, esi", "syscall",
    "test rax, rax", "js 9f", "jz 5f",
    // The parent leaves its child blocked in read, holding a syscall buffer and Arc.
    "xor edi, edi", "jmp 8f",
    "5:",
    "mov eax, 59", "lea rdi, [rip + 7f]", "xor esi, esi", "xor edx, edx", "syscall",
    "mov r12d, 99",
    "9:", "mov edi, r12d",
    "8:", "mov eax, 60", "syscall", "ud2",
    "7:", ".asciz \"/bin/pipe-child37\"",
    "orphan_checks_end:",

    ".global kill_futex, kill_futex_end",
    "kill_futex:",
    "2:",
    "mov eax, 202", "mov edi, 0x600100", "mov esi, 128", "xor edx, edx", "xor r10d, r10d", "syscall",
    "jmp 2b",
    "kill_futex_end:",

    ".global kill_sleep, kill_sleep_end",
    "kill_sleep:",
    "mov qword ptr [0x600100], 86400",
    "2:",
    "mov eax, 35", "mov edi, 0x600100", "xor esi, esi", "syscall",
    "jmp 2b",
    "kill_sleep_end:",

    ".global limit_checks, limit_checks_end",
    "limit_checks:",
    "mov r12d, 1", "mov ebx, 128",
    "2:",
    "mov eax, 56", "mov edi, 0x4111", "xor esi, esi", "syscall",
    "test rax, rax", "js 9f", "jz 5f",
    "dec ebx", "jnz 2b",
    "inc r12d",
    // Completed children still occupy the bounded table until wait4 reaps them.
    "mov eax, 56", "mov edi, 0x4111", "xor esi, esi", "syscall",
    "test rax, rax", "jz 5f", "cmp rax, -11", "jne 9f",
    "inc r12d", "mov ebx, 128",
    "3:",
    "mov eax, 61", "mov edi, -1", "mov esi, 0x600000", "xor edx, edx", "xor r10d, r10d", "syscall",
    "test rax, rax", "jle 9f", "cmp dword ptr [0x600000], 0x2500", "jne 9f",
    "dec ebx", "jnz 3b",
    "inc r12d",
    "mov eax, 61", "mov edi, -1", "xor esi, esi", "xor edx, edx", "xor r10d, r10d", "syscall",
    "cmp rax, -10", "jne 9f",
    "inc r12d",
    // Reaping releases capacity: the next clone and wait must work again.
    "mov eax, 56", "mov edi, 0x4111", "xor esi, esi", "syscall",
    "test rax, rax", "js 9f", "jz 5f", "mov r13, rax",
    "inc r12d",
    "mov eax, 61", "mov rdi, r13", "mov esi, 0x600000", "xor edx, edx", "xor r10d, r10d", "syscall",
    "cmp rax, r13", "jne 9f", "cmp dword ptr [0x600000], 0x2500", "jne 9f",
    "xor edi, edi", "jmp 8f",
    "5:", "mov edi, 37", "jmp 8f",
    "9:", "mov edi, r12d",
    "8:", "mov eax, 60", "syscall", "ud2",
    "limit_checks_end:",
);
