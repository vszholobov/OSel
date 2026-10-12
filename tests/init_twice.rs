#![no_std]
#![no_main]

// A second init would alias the live page table and hand out used frames again

osel::panic_test!("init_twice::is_refused", ["osel::init called twice"], {
    osel::init(osel::boot_info());
});
