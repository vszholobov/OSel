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
pub mod heap;
pub mod exception;
pub mod sync;
