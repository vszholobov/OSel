#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(osel::testosterone::test_runner)]
#![reexport_test_harness_main = "test_main"]

/// Random allocation patterns against the real kernel heap

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;
use osel::memory::HEAP_SIZE;

entry_point!(main, config = &osel::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    osel::init(boot_info);
    test_main();
    osel::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    osel::testosterone::test_panic_handler(info)
}

struct XorShift(u64);

impl XorShift {
    fn next(&mut self, limit: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % limit as u64) as usize
    }
}

#[test_case]
fn random_vectors_keep_their_data() {
    let mut random = XorShift(0x2545_f491_4f6c_dd1d);
    let mut live: Vec<(Vec<u8>, u8)> = Vec::new();
    for step in 0..3000 {
        if live.len() < 40 && random.next(3) != 0 {
            let size = if random.next(8) == 0 { random.next(32 * 1024) + 1 } else { random.next(300) + 1 };
            let fill = step as u8;
            live.push((alloc::vec![fill; size], fill));
        } else if !live.is_empty() {
            let (data, fill) = live.swap_remove(random.next(live.len()));
            assert!(data.iter().all(|&b| b == fill), "corrupted at step {}", step);
        }
    }
}

#[test_case]
fn whole_heap_is_usable_after_small_allocations() {
    // fill the heap with small boxes, free them, then ask for most of the heap at once
    let mut small: Vec<Box<[u8; 48]>> = Vec::with_capacity(8 * 1024);
    while small.len() < small.capacity() {
        small.push(Box::new([7; 48]));
    }
    drop(small);
    let big: Vec<u8> = alloc::vec![1; HEAP_SIZE as usize / 2];
    assert_eq!(big.len(), HEAP_SIZE as usize / 2);
}

#[test_case]
fn growing_string() {
    let mut text = String::new();
    for i in 0..5000 {
        text.push(char::from(b'a' + (i % 26) as u8));
    }
    assert_eq!(text.len(), 5000);
    assert!(text.starts_with("abc"));
}
