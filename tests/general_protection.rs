#![no_std]
#![no_main]

// Selector 0x1234 points into the LDT, which the kernel disables: a general protection
// fault naming the entry, not a read of whatever sits at the LDT reset address 0

osel::panic_test!(
    "general_protection::names_the_selector",
    ["EXCEPTION: GENERAL PROTECTION FAULT", "error code: 0x1234 (LDT entry 582 (0x246))"],
    {
        unsafe { core::arch::asm!("mov ax, 0x1234", "mov ds, ax", out("ax") _) };
    }
);
