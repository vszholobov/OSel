#![no_std]
#![no_main]

// Division by zero in hardware (not Rust's checked division) reaches the panic screen
// with the address of the dividing instruction

osel::panic_test!("divide_error::is_reported", ["EXCEPTION: DIVIDE ERROR"], {
    unsafe {
        core::arch::asm!(
            "lea {rip}, [rip + 2f]",
            "mov [{expected}], {rip}",
            "xor edx, edx",
            "mov eax, 1",
            "xor ecx, ecx",
            "2: div ecx",
            rip = out(reg) _,
            expected = in(reg) osel::testosterone::EXPECTED_RIP.as_ptr(),
            out("eax") _, out("ecx") _, out("edx") _,
        )
    };
});
