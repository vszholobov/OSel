#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(osel::testosterone::test_runner)]
#![reexport_test_harness_main = "test_main"]

/// The ramdisk the runner packs from ramdisk/ reaches the kernel in both boot modes
/// and is mounted as the root file system

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use bootloader_api::info::MemoryRegionKind;
use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;
use osel::fs::{self, Errno, FileType, Inode};
use x86_64::VirtAddr;
use x86_64::structures::paging::Translate;

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

fn ramdisk() -> &'static [u8] {
    fs::ramdisk(osel::boot_info()).expect("the bootloader passed no ramdisk")
}

#[test_case]
fn ramdisk_is_a_cpio_archive() {
    let data = ramdisk();
    assert!(data.starts_with(b"070701"), "no newc magic at the start of the ramdisk");
    assert_eq!(data.len() % 512, 0, "the runner pads the archive to 512 byte blocks");
}

#[test_case]
fn root_file_system_is_mounted() {
    let fs = fs::root().expect("no root file system");
    assert_eq!(fs.name(), "cpio");
    assert_eq!(fs.root().stat().file_type, FileType::Directory);
}

#[test_case]
fn motd_matches_the_repository() {
    let motd = fs::read_file("/etc/motd").expect("cannot read /etc/motd");
    assert_eq!(motd, include_bytes!("../ramdisk/etc/motd"));
}

#[test_case]
fn directories_are_listed() {
    let names = |path| -> Vec<String> {
        fs::list_dir(&*fs::lookup(path).unwrap()).unwrap().into_iter().map(|entry| entry.name).collect()
    };
    assert!(names("/").iter().any(|name| name == "etc"), "{:?}", names("/"));
    assert!(names("/etc").iter().any(|name| name == "motd"), "{:?}", names("/etc"));
}

#[test_case]
fn errors_are_linux_errors() {
    assert_eq!(fs::read_file("/no/such/file").err(), Some(Errno::ENOENT));
    assert_eq!(fs::read_file("/etc/motd/x").err(), Some(Errno::ENOTDIR));
    assert_eq!(fs::read_file("/etc").err(), Some(Errno::EISDIR));
    assert_eq!(fs::lookup("").err(), Some(Errno::ENOENT));
}

/// Reads every file of the ramdisk and checks its size, returns the number of files
fn walk(inode: &dyn Inode) -> usize {
    let stat = inode.stat();
    match stat.file_type {
        FileType::Regular => {
            assert_eq!(fs::read_to_end(inode).unwrap().len() as u64, stat.size);
            1
        }
        FileType::Directory => fs::list_dir(inode)
            .unwrap()
            .iter()
            .map(|entry| walk(&*inode.lookup(&entry.name).unwrap()))
            .sum(),
    }
}

#[test_case]
fn every_file_is_readable() {
    let files = walk(&*fs::lookup("/").unwrap());
    assert!(files >= 1, "the ramdisk has no files");
}

#[test_case]
fn ramdisk_frames_are_not_usable_memory() {
    // a ramdisk frame handed out by the frame allocator would be overwritten
    let regions = &osel::boot_info().memory_regions;
    let data = ramdisk();
    let start = VirtAddr::from_ptr(data.as_ptr());
    let pages = (data.len() as u64).div_ceil(4096);
    for page in 0..pages {
        let virt = start + page * 4096;
        let phys = osel::memory::with_memory(|m| m.mapper.translate_addr(virt))
            .unwrap_or_else(|| panic!("ramdisk page {virt:?} is not mapped"))
            .as_u64();
        let region = regions
            .iter()
            .find(|region| (region.start..region.end).contains(&phys))
            .unwrap_or_else(|| panic!("ramdisk frame {phys:#x} is outside the memory map"));
        assert_ne!(region.kind, MemoryRegionKind::Usable, "ramdisk frame {phys:#x} is usable memory");
    }
}

#[test_case]
fn reading_twice_gives_the_same_bytes() {
    assert_eq!(fs::read_file("/etc/motd"), fs::read_file("/etc/motd"));
}
