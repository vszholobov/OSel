#![no_std]
#![no_main]

// Freeing the page tables the CPU is using would pull the kernel out from under itself

osel::panic_test!("drop_active_space::is_refused", ["dropping the active address space"], {
    let mut space = osel::memory::UserSpace::new().expect("out of frames");
    space.activate();
    drop(space);
});
