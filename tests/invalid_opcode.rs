#![no_std]
#![no_main]

// An undefined instruction reaches the panic screen with its exact address

osel::panic_test!("invalid_opcode::is_reported", ["EXCEPTION: INVALID OPCODE"], {
    unsafe {
        core::arch::asm!(
            "lea {rip}, [rip + 2f]",
            "mov [{expected}], {rip}",
            "2: ud2",
            rip = out(reg) _,
            expected = in(reg) osel::testosterone::EXPECTED_RIP.as_ptr(),
        )
    };
});
