#![no_std]
#![no_main]

// A page fault ends up on the red panic screen with the fault details

const FAULT_ADDRESS: u64 = 0xdead_beef_000;

osel::panic_test!(
    "panic_screen::page_fault_is_shown",
    ["EXCEPTION: PAGE FAULT", "accessed address: 0xdeadbeef000", "page not present on read by kernel"],
    {
        unsafe { core::ptr::read_volatile(FAULT_ADDRESS as *const u64) };
    }
);
