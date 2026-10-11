#![no_std]
#![no_main]

// Every CPU exception has a handler: vector 4 is reported by name, not as a missing gate

osel::panic_test!("overflow::has_a_handler", ["EXCEPTION: OVERFLOW"], {
    unsafe { core::arch::asm!("int 4") };
});
