//! Platform independent parts of the OSel kernel.
//!
//! Nothing here touches hardware, so the code builds for the host as well and is
//! covered by plain `cargo test-core` without booting QEMU.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod console;
pub mod keyboard;
pub mod pit;
pub mod memory;
pub mod frames;
pub mod address_space;
pub mod elf;
pub mod initial_stack;
pub mod trap;
pub mod syscall;
pub mod user_mem;
pub mod heap;
pub mod exception;
pub mod sync;
pub mod errno;
pub mod block;
pub mod vfs;
pub mod cpio;
