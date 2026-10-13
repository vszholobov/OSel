//! The first program for OSel: prints a line through `write` and exits through `exit`.
//! Written in assembly on purpose: it checks the Linux system call interface itself,
//! without a libc in between. The runner builds it and puts it into the ramdisk as
//! `/bin/hello`.

#![no_std]
#![no_main]

core::arch::global_asm!(
    ".section .rodata",
    "message:",
    ".ascii \"Hello from ring 3!\\n\"",
    "message_end:",
    ".text",
    ".globl _start",
    "_start:",
    // write(1, message, length)
    "mov edi, 1",
    "lea rsi, [rip + message]",
    // in Intel syntax a bare symbol is a memory operand, so the length is computed
    "lea rdx, [rip + message_end]",
    "sub rdx, rsi",
    "mov eax, 1",
    "syscall",
    // exit(0)
    "xor edi, edi",
    "mov eax, 60",
    "syscall",
    "ud2",
);

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
