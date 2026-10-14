//! Platform independent parts of the OSel kernel.
//!
//! Nothing here touches hardware, so the code builds for the host as well and is
//! covered by plain `cargo test-core` without booting QEMU.

#![cfg_attr(not(test), no_std)]

extern crate alloc;
pub mod net;

pub mod address_space;
pub mod block;
pub mod clock;
pub mod console;
pub mod cpio;
pub mod devices;
pub mod elf;
pub mod errno;
pub mod exception;
pub mod file;
pub mod font;
pub mod fpu;
pub mod frames;
pub mod futex;
pub mod heap;
pub mod initial_stack;
pub mod keyboard;
pub mod memory;
pub mod mmap;
pub mod pipe;
pub mod pit;
pub mod poll;
pub mod ps2;
pub mod sched;
pub mod shell;
pub mod sleeper;
pub mod sync;
pub mod syscall;
pub mod terminal;
pub mod trap;
pub mod user_mem;
pub mod vfs;
pub mod vma;
