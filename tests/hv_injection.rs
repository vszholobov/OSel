#![no_std]
#![no_main]

// Vector 28 (AMD SEV-SNP) has its own handler and is reported by name. Vector 29 can't be
// raised the same way: a software `int` pushes no error code, and its handler expects one.

osel::panic_test!("hv_injection::has_a_handler", ["EXCEPTION: HYPERVISOR INJECTION EXCEPTION"], {
    unsafe { core::arch::asm!("int 28") };
});
