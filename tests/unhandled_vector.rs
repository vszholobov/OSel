#![no_std]
#![no_main]

// An interrupt vector without a handler faults on the missing IDT gate

osel::panic_test!("unhandled_vector::names_the_vector", ["EXCEPTION: SEGMENT NOT PRESENT", "IDT entry 80 (0x50)"], {
    unsafe { core::arch::asm!("int 0x50") };
});
