//! program module loads executables from the file system into address spaces
//!
//! Parsing ELF and laying out the initial stack live in `osel_core::elf` and
//! `osel_core::initial_stack`; here they meet a real `UserSpace`.

use crate::fs;
use crate::memory::{PAGE_SIZE, USER_END, UserSpace};
use osel_core::elf::{self, ProgramMemory, ReadAt};
use osel_core::errno::{Errno, KResult};
use osel_core::initial_stack::{self, AuxValues};
use x86_64::VirtAddr;
use x86_64::structures::paging::PageTableFlags;

/// Top of the main thread's stack. The last page of the lower half stays unmapped: no
/// user instruction ends right at the non-canonical boundary, which `sysret` can't return to.
pub const STACK_TOP: u64 = USER_END - PAGE_SIZE;
/// Mapped right away until page faults map memory on demand; Go's main thread needs 64 KiB
pub const STACK_SIZE: u64 = 256 * 1024;
/// Lowest address of the stack; the page below it is a guard, segments end before it
pub const STACK_BOTTOM: u64 = STACK_TOP - STACK_SIZE;

/// A program ready to run: its address space, where it starts and its stack pointer
pub struct Loaded {
    pub space: UserSpace,
    pub entry: u64,
    pub rsp: u64,
}

impl ProgramMemory for UserSpace {
    fn map_zeroed(&mut self, start: u64, size: u64, flags: PageTableFlags) -> KResult<()> {
        Ok(self.map(VirtAddr::try_new(start).map_err(|_| Errno::EINVAL)?, size, flags)?)
    }

    fn write(&mut self, address: u64, bytes: &[u8]) -> KResult<()> {
        self.write_bytes(address, bytes)
    }
}

/// Loads the executable at `path` with its arguments and environment
pub fn load(path: &str, argv: &[&[u8]], envp: &[&[u8]]) -> KResult<Loaded> {
    let inode = fs::lookup(path)?;
    load_file(&*inode, argv, envp)
}

/// Loads an executable into a new address space and builds its initial stack
pub fn load_file(file: &(impl ReadAt + ?Sized), argv: &[&[u8]], envp: &[&[u8]]) -> KResult<Loaded> {
    let mut space = UserSpace::new().ok_or(Errno::ENOMEM)?;
    let program = elf::load(file, &mut space, STACK_BOTTOM - PAGE_SIZE)?;
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::NO_EXECUTE;
    space.map_zeroed(STACK_BOTTOM, STACK_SIZE, flags)?;
    let aux = AuxValues { phdr: program.phdr, phnum: program.phnum, entry: program.entry };
    let image = initial_stack::build(STACK_TOP, argv, envp, &aux, random_bytes())?;
    space.write_bytes(image.rsp, &image.bytes)?;
    Ok(Loaded { space, entry: program.entry, rsp: image.rsp })
}

/// Bytes for `AT_RANDOM`: Go seeds its hash maps with them. Not cryptographic: the time
/// stamp counter mixed with splitmix64; `RDRAND` is not on every CPU QEMU emulates.
fn random_bytes() -> [u8; 16] {
    let mut state = unsafe { core::arch::x86_64::_rdtsc() };
    let mut next = || {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    };
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&next().to_le_bytes());
    bytes[8..].copy_from_slice(&next().to_le_bytes());
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{total_usable_frames, used_frames, with_memory};
    use alloc::vec::Vec;
    use osel_core::elf::{ElfBuilder, PF_R, PF_W, PF_X};
    use x86_64::structures::paging::{FrameAllocator, FrameDeallocator};

    const TEXT: u64 = 0x40_0000;
    const DATA: u64 = 0x60_0000;

    fn program() -> Vec<u8> {
        ElfBuilder::new(TEXT).segment(TEXT, PF_R | PF_X, &[0xeb, 0xfe], 2).segment(DATA, PF_R | PF_W, b"data", 0x3000).build()
    }

    fn read(space: &mut UserSpace, address: u64, len: usize) -> Vec<u8> {
        let mut buffer = alloc::vec![0u8; len];
        space.read_bytes(address, &mut buffer).unwrap();
        buffer
    }

    fn flags(space: &mut UserSpace, address: u64) -> PageTableFlags {
        space.page_flags(VirtAddr::new(address)).expect("not mapped")
    }

    #[test_case]
    fn test_a_program_is_loaded_with_its_stack() {
        let file = program();
        let mut loaded = load_file(&file[..], &[b"/bin/test", b"x"], &[b"A=1"]).unwrap();
        assert_eq!(loaded.entry, TEXT);
        assert_eq!(read(&mut loaded.space, TEXT, 3), [0xeb, 0xfe, 0]);
        assert_eq!(read(&mut loaded.space, DATA, 5), *b"data\0");
        assert!(read(&mut loaded.space, DATA + 4, 0x3000 - 4).iter().all(|&b| b == 0), ".bss is zero");
        let text = flags(&mut loaded.space, TEXT);
        assert!(text.contains(PageTableFlags::USER_ACCESSIBLE) && !text.contains(PageTableFlags::WRITABLE));
        assert!(!text.contains(PageTableFlags::NO_EXECUTE));
        let data = flags(&mut loaded.space, DATA + 0x2000);
        assert!(data.contains(PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE));

        assert_eq!(loaded.rsp % 16, 0);
        assert!(loaded.rsp > STACK_BOTTOM && loaded.rsp < STACK_TOP);
        let argc = u64::from_le_bytes(read(&mut loaded.space, loaded.rsp, 8).try_into().unwrap());
        assert_eq!(argc, 2);
        let argv0 = u64::from_le_bytes(read(&mut loaded.space, loaded.rsp + 8, 8).try_into().unwrap());
        assert_eq!(read(&mut loaded.space, argv0, 10), *b"/bin/test\0");
        assert!(loaded.space.translate(VirtAddr::new(STACK_TOP)).is_none(), "the top page stays unmapped");
        assert!(loaded.space.translate(VirtAddr::new(STACK_BOTTOM - 1)).is_none(), "guard page");
        let kernel = VirtAddr::from_ptr(&STACK_TOP).as_u64();
        assert!(matches!(loaded.space.read_bytes(kernel, &mut [0; 8]), Err(Errno::EFAULT)), "kernel memory through a user space");
        assert!(matches!(loaded.space.write_bytes(USER_END - 4, &[0; 8]), Err(Errno::EFAULT)), "across the end of the lower half");
    }

    #[test_case]
    fn test_a_broken_program_is_refused() {
        let mut file = program();
        file[18] = 183;
        assert!(matches!(load_file(&file[..], &[b"x"], &[]), Err(Errno::ENOEXEC)));
        // a segment reaching into the stack
        let big = ElfBuilder::new(TEXT).segment(TEXT, PF_R | PF_X, &[0xc3], STACK_BOTTOM - TEXT).build();
        assert!(matches!(load_file(&big[..], &[b"x"], &[]), Err(Errno::ENOEXEC)));
        assert!(matches!(load("/no/such/program", &[b"x"], &[]), Err(Errno::ENOENT)));
    }

    #[test_case]
    fn test_a_segment_may_reach_the_stack_guard() {
        let limit = STACK_BOTTOM - PAGE_SIZE;
        let file = ElfBuilder::new(TEXT).segment(TEXT, PF_R | PF_X, &[0xc3], 1).segment(limit - 0x1000, PF_R | PF_W, b"", 0x1000).build();
        let mut loaded = load_file(&file[..], &[b"x"], &[]).unwrap();
        assert!(loaded.space.translate(VirtAddr::new(limit - 1)).is_some());
        assert!(loaded.space.translate(VirtAddr::new(limit)).is_none(), "the guard stays unmapped");
    }

    /// Nikka 4-process-2 `create_process_no_frame`: running out of frames at every step
    /// of loading leaks nothing and reports `ENOMEM`
    #[test_case]
    fn test_loading_out_of_frames_at_every_step() {
        let file = program();
        let used = used_frames();
        let loaded = load_file(&file[..], &[b"x"], &[]).unwrap();
        let needed = used_frames() - used;
        drop(loaded);
        let mut stolen = Vec::with_capacity(total_usable_frames());
        with_memory(|m| {
            while let Some(frame) = m.frame_allocator.allocate_frame() {
                stolen.push(frame);
            }
        });
        // only `available` frames move back and forth, not all of memory every time
        for available in 0..needed {
            with_memory(|m| {
                for frame in stolen.drain(stolen.len() - available..) {
                    unsafe { m.frame_allocator.deallocate_frame(frame) };
                }
            });
            let guard = crate::testosterone::forbid_frame_leaks();
            assert!(matches!(load_file(&file[..], &[b"x"], &[]), Err(Errno::ENOMEM)), "{} frames were enough", available);
            drop(guard);
            with_memory(|m| {
                for _ in 0..available {
                    stolen.push(m.frame_allocator.allocate_frame().expect("the frames came back"));
                }
                assert!(m.frame_allocator.allocate_frame().is_none());
            });
        }
        with_memory(|m| {
            for frame in stolen.drain(..) {
                unsafe { m.frame_allocator.deallocate_frame(frame) };
            }
        });
    }
}
