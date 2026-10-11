#![no_std]
#![no_main]

// Division by zero in hardware (not Rust's checked division) reaches the panic screen

osel::panic_test!("divide_error::is_reported", ["EXCEPTION: DIVIDE ERROR", "rip: 0xffff8"], {
    unsafe {
        core::arch::asm!("xor edx, edx", "mov eax, 1", "xor ecx, ecx", "div ecx", out("eax") _, out("ecx") _, out("edx") _)
    };
});
