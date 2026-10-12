//! fs module mounts the ramdisk as the root file system and reads files from it
//!
//! The interfaces (`BlockDevice`, `FileSystem`, `Inode`) and the cpio format live
//! in `osel_core`; here is only what depends on the boot: where the ramdisk is.

use crate::sync::IrqSpinlock;
use alloc::sync::Arc;
use alloc::vec::Vec;
use bootloader_api::BootInfo;
use osel_core::block::MemBlockDevice;
use osel_core::cpio::CpioFs;

pub use osel_core::errno::{Errno, KResult};
pub use osel_core::vfs::*;

static ROOT_FS: IrqSpinlock<Option<Arc<dyn FileSystem>>> = IrqSpinlock::new("ROOT_FS", None);

/// Mounts the ramdisk as `/`. Without a ramdisk the kernel runs without files;
/// a ramdisk that is not a cpio archive is a bug of the image and a panic.
pub(crate) fn init(boot_info: &'static BootInfo) {
    let Some(data) = ramdisk(boot_info) else { return };
    let fs = CpioFs::mount(Arc::new(MemBlockDevice::new(data)))
        .unwrap_or_else(|e| panic!("the ramdisk is not a valid cpio archive: {e}"));
    *ROOT_FS.lock() = Some(Arc::new(fs));
}

/// The bytes of the ramdisk the bootloader loaded, `None` if there is none
pub fn ramdisk(boot_info: &'static BootInfo) -> Option<&'static [u8]> {
    let address = boot_info.ramdisk_addr.into_option()?;
    let len = usize::try_from(boot_info.ramdisk_len).ok().filter(|&len| len > 0)?;
    // SAFETY: the bootloader maps the ramdisk at this address for the whole run and
    // marks its frames as its own, so the frame allocator never hands them out
    Some(unsafe { core::slice::from_raw_parts(address as *const u8, len) })
}

/// The root file system, `None` if the kernel booted without a ramdisk
pub fn root() -> Option<Arc<dyn FileSystem>> {
    ROOT_FS.lock().clone()
}

/// Finds the inode at the absolute `path`; `ENOENT` if there is no root file system
pub fn lookup(path: &str) -> KResult<Arc<dyn Inode>> {
    let fs = root().ok_or(Errno::ENOENT)?;
    resolve_absolute(&fs.root(), path)
}

/// Reads the whole file at `path`
pub fn read_file(path: &str) -> KResult<Vec<u8>> {
    read_to_end(&*lookup(path)?)
}
