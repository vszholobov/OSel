#![no_std]
#![no_main]

// An undefined instruction reaches the panic screen

osel::panic_test!("invalid_opcode::is_reported", ["EXCEPTION: INVALID OPCODE"], {
    unsafe { core::arch::asm!("ud2") };
});
