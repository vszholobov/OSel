#![no_std]
#![no_main]

// Read-only kernel data really is read-only: writing it is a protection violation

static READ_ONLY: u64 = 42;

osel::panic_test!("write_protection::rodata_is_read_only", ["EXCEPTION: PAGE FAULT", "protection violation on write by kernel"], {
    // a plain instruction: in Rust a write through a shared reference is undefined behaviour
    unsafe { core::arch::asm!("mov qword ptr [{}], 7", in(reg) &READ_ONLY) };
});
