//! Hardware drivers. Existing public module names are re-exported by the kernel.
pub mod avganec;
pub mod cereal;
pub mod keyboard;
pub mod pci;
pub mod e1000e;
mod dma;
pub mod rtl8168;
pub mod ethernet;
pub mod qca6174;
