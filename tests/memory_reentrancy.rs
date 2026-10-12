#![no_std]
#![no_main]

// Taking the memory lock twice is a clear panic, not a silent hang with interrupts off

osel::panic_test!("memory_reentrancy::is_reported", ["deadlock on lock `MEMORY`", "held since tests/memory_reentrancy.rs"], {
    osel::memory::with_memory(|_| osel::memory::with_memory(|_| ()));
});
