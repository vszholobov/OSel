#![no_std]
#![no_main]

// Allocating while the heap lock is held would spin forever with interrupts off;
// the lock reports it as a deadlock naming both places instead

extern crate alloc;

osel::panic_test!("deadlock::is_reported", ["deadlock on lock `ALLOCATOR`", "held since tests/deadlock.rs", "waiting at src/allocator.rs"], {
    osel::allocator::with_allocator(|_| {
        let boxed = alloc::boxed::Box::new(1);
        core::hint::black_box(boxed);
    });
});
