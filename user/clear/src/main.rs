//! `/bin/clear`: a standalone program, launched by the game's existing `os/exec` call.

#![no_std]
#![no_main]

core::arch::global_asm!(
    ".section .rodata",
    "sequence:",
    ".byte 27, 91, 72, 27, 91, 50, 74",
    ".text",
    ".globl _start",
    "_start:",
    "mov edi, 1",
    "lea rsi, [rip + sequence]",
    "mov edx, 7",
    "2:",
    "mov eax, 1",
    "syscall",
    "cmp rax, -4", // EINTR: retry the remaining bytes
    "je 2b",
    "test rax, rax",
    "jle 3f",
    "add rsi, rax",
    "sub rdx, rax",
    "jnz 2b",
    "xor edi, edi",
    "jmp 4f",
    "3:",
    "mov edi, 1",
    "4:",
    "mov eax, 60",
    "syscall",
    "ud2",
);

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
