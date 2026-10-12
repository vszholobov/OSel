#![no_std]
#![no_main]

// Running out of heap is a panic with a readable message, not a hang

extern crate alloc;

osel::panic_test!("heap_oom::is_reported", ["memory allocation of 1048577 bytes failed"], {
    let all = alloc::vec![0u8; osel::memory::HEAP_SIZE as usize + 1];
    core::hint::black_box(all);
});
