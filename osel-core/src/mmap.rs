//! Memory a program asks for: `mmap`, `munmap`, `mprotect`, `madvise` and pages mapped on
//! the first touch
//!
//! `Mappings` keeps the areas of an address space (`VmaSet`) and changes its page tables
//! through `PageTables`. Only anonymous private memory: a new area has no pages at all, the
//! page fault of the first touch maps a zeroed frame. So Go's gigabyte of `PROT_NONE`
//! reservations costs nothing.
//!
//! The rights of an area live in the leaf entries of its pages. A `PROT_NONE` page that
//! already has a frame keeps it in an entry without `PRESENT` (like Linux): its data comes
//! back with `mprotect`, and `munmap` or the end of the program frees the frame.

use crate::elf::MIN_USER_ADDR;
use crate::errno::{Errno, KResult};
use crate::memory::{PAGE_SIZE, USER_END};
use crate::vma::{PROT_EXEC, PROT_READ, PROT_WRITE, Vma, VmaSet};
use x86_64::structures::paging::PageTableFlags;

pub const MAP_SHARED: u64 = 0x01;
pub const MAP_PRIVATE: u64 = 0x02;
pub const MAP_SHARED_VALIDATE: u64 = 0x03;
const MAP_TYPE: u64 = 0x0f;
pub const MAP_FIXED: u64 = 0x10;
pub const MAP_ANONYMOUS: u64 = 0x20;
pub const MAP_FIXED_NOREPLACE: u64 = 0x10_0000;

pub const MADV_DONTNEED: u64 = 4;
pub const MADV_FREE: u64 = 8;
pub const MADV_NOHUGEPAGE: u64 = 15;
/// Only for shared memory: `EINVAL` on private memory, like Linux
const MADV_REMOVE: u64 = 9;
const MADV_DONTNEED_LOCKED: u64 = 24;
/// The last advice Linux knows besides the hardware poison ones
const MADV_LAST: u64 = 25;

/// `mmap` without a fixed address places areas below this, as high as possible; the main
/// thread's stack lives above
pub const MMAP_TOP: u64 = USER_END - (1 << 30);

/// Leaf flags of a `PROT_NONE` page that has a frame: not present, and marked for whoever
/// reads the table
pub const PROT_NONE_FLAGS: PageTableFlags = PageTableFlags::BIT_9.union(PageTableFlags::NO_EXECUTE);

const ALL_PROT: u32 = PROT_READ | PROT_WRITE | PROT_EXEC;

/// The page tables of the address space the areas belong to
pub trait PageTables {
    /// Whether the page at `page` has a frame, present or not
    fn has_frame(&mut self, page: u64) -> bool;
    /// Maps a zeroed frame at `page`; `false` if there is no frame
    fn map_zeroed(&mut self, page: u64, flags: PageTableFlags) -> bool;
    /// Unmaps the pages of `start..end` and frees their frames; flushes the TLB
    fn unmap(&mut self, start: u64, end: u64);
    /// Gives the pages of `start..end` that have a frame the flags `flags`; flushes the TLB
    fn protect(&mut self, start: u64, end: u64, flags: PageTableFlags);
}

/// Leaf flags of a page with the rights `prot`. x86 can't have a writable or executable
/// page that is not readable, so any right gives reading.
pub fn flags_of(prot: u32) -> PageTableFlags {
    if prot & ALL_PROT == 0 {
        return PROT_NONE_FLAGS;
    }
    let mut flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
    if prot & PROT_WRITE != 0 {
        flags |= PageTableFlags::WRITABLE;
    }
    if prot & PROT_EXEC == 0 {
        flags |= PageTableFlags::NO_EXECUTE;
    }
    flags
}

/// Rights of pages mapped with `flags`
pub fn prot_of(flags: PageTableFlags) -> u32 {
    if !flags.contains(PageTableFlags::PRESENT) {
        return 0;
    }
    let mut prot = PROT_READ;
    if flags.contains(PageTableFlags::WRITABLE) {
        prot |= PROT_WRITE;
    }
    if !flags.contains(PageTableFlags::NO_EXECUTE) {
        prot |= PROT_EXEC;
    }
    prot
}

/// How a program touched a page
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Touch {
    Read,
    Write,
    Execute,
}

fn allows(prot: u32, touch: Touch) -> bool {
    match touch {
        Touch::Read => prot & ALL_PROT != 0,
        Touch::Write => prot & PROT_WRITE != 0,
        Touch::Execute => prot & PROT_EXEC != 0,
    }
}

/// `len` rounded up to whole pages, `None` on overflow
fn page_align(len: u64) -> Option<u64> {
    len.checked_add(PAGE_SIZE - 1).map(|len| len & !(PAGE_SIZE - 1))
}

/// The areas of one address space
#[derive(Debug, Clone)]
pub struct Mappings {
    areas: VmaSet,
}

impl Default for Mappings {
    fn default() -> Self {
        Mappings::new()
    }
}

impl Mappings {
    pub fn new() -> Mappings {
        Mappings { areas: VmaSet::new(MIN_USER_ADDR, MMAP_TOP) }
    }

    pub fn areas(&self) -> impl Iterator<Item = &Vma> {
        self.areas.areas()
    }

    /// The area holding `address`
    pub fn find(&self, address: u64) -> Option<Vma> {
        self.areas.find(address)
    }

    /// Records pages the kernel mapped right away (program segments, the stack), so that
    /// the program can `munmap` or `mprotect` them like its own
    pub fn record(&mut self, start: u64, end: u64, flags: PageTableFlags) {
        self.areas.insert(start, end, prot_of(flags));
    }

    /// Forgets pages the kernel unmapped
    pub fn forget(&mut self, start: u64, end: u64) {
        self.areas.remove(start, end);
    }

    /// `mmap` of anonymous memory, errors in Linux's order. Unknown `prot` bits are `EINVAL`,
    /// Linux ignores them. A file mapping is `ENODEV`; the caller checks the descriptor
    /// before (`EBADF`). Returns the address.
    pub fn mmap(&mut self, tables: &mut impl PageTables, addr: u64, len: u64, prot: u32, flags: u64) -> KResult<u64> {
        if len == 0 || prot & !ALL_PROT != 0 {
            return Err(Errno::EINVAL);
        }
        if !matches!(flags & MAP_TYPE, MAP_SHARED | MAP_PRIVATE | MAP_SHARED_VALIDATE) {
            return Err(Errno::EINVAL);
        }
        // there is no fork, so shared anonymous memory is the same as private
        if flags & MAP_ANONYMOUS == 0 {
            return Err(Errno::ENODEV);
        }
        let len = page_align(len).ok_or(Errno::ENOMEM)?;
        if flags & (MAP_FIXED | MAP_FIXED_NOREPLACE) == 0 {
            // a hint is taken if it is free, rounded down to a page like Linux does
            let hint = (addr >= MIN_USER_ADDR).then_some(addr & !(PAGE_SIZE - 1));
            let start = self.areas.find_free(len, hint).ok_or(Errno::ENOMEM)?;
            self.areas.insert(start, start + len, prot);
            return Ok(start);
        }
        if len > USER_END || addr > USER_END - len {
            return Err(Errno::ENOMEM);
        }
        if addr % PAGE_SIZE != 0 {
            return Err(Errno::EINVAL);
        }
        if addr < MIN_USER_ADDR {
            return Err(Errno::EPERM);
        }
        let end = addr + len;
        if flags & MAP_FIXED_NOREPLACE != 0 && !self.areas.is_free(addr, end) {
            return Err(Errno::EEXIST);
        }
        // whatever was there goes: Go commits memory inside its reservations like this
        self.areas.insert(addr, end, prot);
        tables.unmap(addr, end);
        Ok(addr)
    }

    /// `munmap`: a range with nothing mapped is fine
    pub fn munmap(&mut self, tables: &mut impl PageTables, addr: u64, len: u64) -> KResult<()> {
        if addr % PAGE_SIZE != 0 {
            return Err(Errno::EINVAL);
        }
        let len = page_align(len).filter(|&len| len != 0).ok_or(Errno::EINVAL)?;
        if len > USER_END || addr > USER_END - len {
            return Err(Errno::EINVAL);
        }
        self.areas.remove(addr, addr + len);
        tables.unmap(addr, addr + len);
        Ok(())
    }

    /// `mprotect`: every page of the range must be mapped (`ENOMEM`, and nothing changes)
    pub fn mprotect(&mut self, tables: &mut impl PageTables, addr: u64, len: u64, prot: u32) -> KResult<()> {
        if addr % PAGE_SIZE != 0 {
            return Err(Errno::EINVAL);
        }
        if len == 0 {
            return Ok(());
        }
        if prot & !ALL_PROT != 0 {
            return Err(Errno::EINVAL);
        }
        let end = page_align(len).and_then(|len| addr.checked_add(len)).ok_or(Errno::ENOMEM)?;
        if end > USER_END {
            return Err(Errno::ENOMEM);
        }
        self.areas.protect(addr, end, prot).ok_or(Errno::ENOMEM)?;
        tables.protect(addr, end, flags_of(prot));
        Ok(())
    }

    /// `madvise`: `MADV_DONTNEED` and `MADV_FREE` drop the pages, so the next touch gets
    /// zeros (Go relies on that); other advice changes nothing. A range with unmapped
    /// pages is `ENOMEM` after the mapped ones were handled, like Linux.
    pub fn madvise(&mut self, tables: &mut impl PageTables, addr: u64, len: u64, advice: u64) -> KResult<()> {
        let known = advice <= MADV_DONTNEED || (MADV_FREE..=MADV_LAST).contains(&advice);
        if addr % PAGE_SIZE != 0 || !known || advice == MADV_REMOVE {
            return Err(Errno::EINVAL);
        }
        if len == 0 {
            return Ok(());
        }
        let end = page_align(len).and_then(|len| addr.checked_add(len)).ok_or(Errno::EINVAL)?;
        let end_in_user = end.min(USER_END);
        if matches!(advice, MADV_DONTNEED | MADV_FREE | MADV_DONTNEED_LOCKED) && addr < end_in_user {
            tables.unmap(addr, end_in_user);
        }
        if end > USER_END || !self.areas.covered(addr, end) {
            return Err(Errno::ENOMEM);
        }
        Ok(())
    }

    /// A page fault at `address`: maps a zeroed page if the address is in an area that
    /// allows `touch` and the page has no frame yet. `false` if the fault is the
    /// program's (or there is no frame left): it ends.
    pub fn fault(&mut self, tables: &mut impl PageTables, address: u64, touch: Touch) -> bool {
        let Some(vma) = self.areas.find(address) else {
            return false;
        };
        let page = address & !(PAGE_SIZE - 1);
        if !allows(vma.prot, touch) || tables.has_frame(page) {
            return false;
        }
        tables.map_zeroed(page, flags_of(vma.prot))
    }

    /// Maps the untouched pages of `len` bytes at `start` that the kernel is about to read
    /// (`Touch::Read`) or write for the program, as the program's own touch would. Stops
    /// at the first page the program may not touch so: the check after the call fails there.
    pub fn fault_in(&mut self, tables: &mut impl PageTables, start: u64, len: u64, touch: Touch) {
        let end = start.saturating_add(len).min(USER_END);
        let mut page = start & !(PAGE_SIZE - 1);
        while page < end {
            match self.areas.find(page) {
                Some(vma) if allows(vma.prot, touch) => {
                    if !tables.has_frame(page) && !tables.map_zeroed(page, flags_of(vma.prot)) {
                        return;
                    }
                }
                _ => return,
            }
            page += PAGE_SIZE;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;
    use alloc::vec::Vec;

    const P: u64 = PAGE_SIZE;
    const RW: u32 = PROT_READ | PROT_WRITE;
    const ANON: u64 = MAP_PRIVATE | MAP_ANONYMOUS;
    const MIB: u64 = 1 << 20;

    /// Page tables as a map of page → (flags, a byte standing for the data)
    #[derive(Default)]
    struct Fake {
        pages: BTreeMap<u64, (PageTableFlags, u8)>,
        frames_left: Option<usize>,
        flushes: usize,
    }

    impl PageTables for Fake {
        fn has_frame(&mut self, page: u64) -> bool {
            self.pages.contains_key(&page)
        }

        fn map_zeroed(&mut self, page: u64, flags: PageTableFlags) -> bool {
            assert!(!self.pages.contains_key(&page), "mapped twice");
            assert_eq!(page % P, 0);
            if let Some(left) = &mut self.frames_left {
                if *left == 0 {
                    return false;
                }
                *left -= 1;
            }
            self.pages.insert(page, (flags, 0));
            true
        }

        fn unmap(&mut self, start: u64, end: u64) {
            self.pages.retain(|&page, _| page < start || end <= page);
            self.flushes += 1;
        }

        fn protect(&mut self, start: u64, end: u64, flags: PageTableFlags) {
            for (_, page) in self.pages.range_mut(start..end) {
                page.0 = flags;
            }
            self.flushes += 1;
        }
    }

    impl Fake {
        /// A program's write: faults the page in if needed, then stores `value`
        fn store(&mut self, mappings: &mut Mappings, address: u64, value: u8) -> bool {
            let page = address & !(P - 1);
            let writable = |pages: &BTreeMap<u64, (PageTableFlags, u8)>| {
                pages.get(&page).is_some_and(|(flags, _)| flags.contains(PageTableFlags::PRESENT | PageTableFlags::WRITABLE))
            };
            if !writable(&self.pages) && !(mappings.fault(self, address, Touch::Write) && writable(&self.pages)) {
                return false;
            }
            self.pages.get_mut(&page).unwrap().1 = value;
            true
        }

        /// A program's instruction fetch
        fn fetch(&mut self, mappings: &mut Mappings, address: u64) -> bool {
            let page = address & !(P - 1);
            let runs = |pages: &BTreeMap<u64, (PageTableFlags, u8)>| {
                pages.get(&page).is_some_and(|(flags, _)| flags.contains(PageTableFlags::PRESENT) && !flags.contains(PageTableFlags::NO_EXECUTE))
            };
            runs(&self.pages) || (mappings.fault(self, address, Touch::Execute) && runs(&self.pages))
        }

        /// A program's read, `None` if it faults for good
        fn load(&mut self, mappings: &mut Mappings, address: u64) -> Option<u8> {
            let page = address & !(P - 1);
            let present = |pages: &BTreeMap<u64, (PageTableFlags, u8)>| {
                pages.get(&page).is_some_and(|(flags, _)| flags.contains(PageTableFlags::PRESENT))
            };
            if !present(&self.pages) && !(mappings.fault(self, address, Touch::Read) && present(&self.pages)) {
                return None;
            }
            Some(self.pages[&page].1)
        }
    }

    #[test]
    fn flags_and_rights() {
        use PageTableFlags as F;
        let user = F::PRESENT | F::USER_ACCESSIBLE;
        assert_eq!(flags_of(0), PROT_NONE_FLAGS);
        assert!(!PROT_NONE_FLAGS.contains(F::PRESENT));
        assert_eq!(flags_of(PROT_READ), user | F::NO_EXECUTE);
        assert_eq!(flags_of(RW), user | F::WRITABLE | F::NO_EXECUTE);
        assert_eq!(flags_of(PROT_WRITE), user | F::WRITABLE | F::NO_EXECUTE);
        assert_eq!(flags_of(PROT_READ | PROT_EXEC), user);
        assert_eq!(flags_of(ALL_PROT), user | F::WRITABLE);
        for prot in [0, PROT_READ, RW, PROT_READ | PROT_EXEC, ALL_PROT] {
            assert_eq!(prot_of(flags_of(prot)), prot, "{prot}");
        }
    }

    #[test]
    fn anonymous_memory_is_lazy_and_zero() {
        let (mut m, mut t) = (Mappings::new(), Fake::default());
        let addr = m.mmap(&mut t, 0, 64 * MIB, RW, ANON).unwrap();
        assert_eq!(addr, MMAP_TOP - 64 * MIB, "from the top");
        assert!(t.pages.is_empty(), "nothing mapped yet");
        assert_eq!(t.load(&mut m, addr + 5 * P + 7), Some(0));
        assert!(t.store(&mut m, addr + 9 * P, 42));
        assert_eq!(t.load(&mut m, addr + 9 * P), Some(42));
        assert_eq!(t.load(&mut m, addr + 64 * MIB - 1), Some(0), "the last byte");
        assert_eq!(t.pages.len(), 3, "only the touched pages");
        assert_eq!(t.load(&mut m, addr + 64 * MIB), None, "one byte past the area");
        assert_eq!(t.load(&mut m, addr - 1), None, "one byte before it");
        let len = m.mmap(&mut t, 0, 10, RW, ANON).unwrap();
        assert_eq!(len, addr - P, "the length is rounded up to a page");
    }

    #[test]
    fn rights_of_a_fault() {
        let (mut m, mut t) = (Mappings::new(), Fake::default());
        let none = m.mmap(&mut t, 0, P, 0, ANON).unwrap();
        let read = m.mmap(&mut t, 0, P, PROT_READ, ANON).unwrap();
        let code = m.mmap(&mut t, 0, P, PROT_READ | PROT_EXEC, ANON).unwrap();
        let data = m.mmap(&mut t, 0, P, RW, ANON).unwrap();
        assert!(!m.fault(&mut t, none, Touch::Read));
        assert!(!m.fault(&mut t, read, Touch::Write));
        assert!(!m.fault(&mut t, data, Touch::Execute), "no execution of data");
        assert!(m.fault(&mut t, code, Touch::Execute));
        assert_eq!(t.pages[&code].0, flags_of(PROT_READ | PROT_EXEC));
        assert!(m.fault(&mut t, read, Touch::Read));
        assert!(!m.fault(&mut t, read, Touch::Read), "a page with a frame is not mapped again");
        assert!(!m.fault(&mut t, MIN_USER_ADDR - P, Touch::Read), "outside of every area");
        assert_eq!(t.pages.len(), 2);
    }

    #[test]
    fn no_frames_left_ends_the_program() {
        let (mut m, mut t) = (Mappings::new(), Fake { frames_left: Some(1), ..Fake::default() });
        let addr = m.mmap(&mut t, 0, 4 * P, RW, ANON).unwrap();
        assert!(m.fault(&mut t, addr, Touch::Write));
        assert!(!m.fault(&mut t, addr + P, Touch::Write));
    }

    #[test]
    fn mmap_errors_in_linux_order() {
        let (mut m, mut t) = (Mappings::new(), Fake::default());
        let fixed = ANON | MAP_FIXED;
        let cases = [
            (0, 0, RW, ANON, Errno::EINVAL),
            (0, P, 8, ANON, Errno::EINVAL),
            (0, P, RW, MAP_ANONYMOUS, Errno::EINVAL),
            (0, P, RW, MAP_ANONYMOUS | 0x0f, Errno::EINVAL),
            (0, P, RW, MAP_PRIVATE, Errno::ENODEV),
            (0, u64::MAX, RW, ANON, Errno::ENOMEM),
            (0, USER_END, RW, ANON, Errno::ENOMEM),
            (USER_END - P, 2 * P, RW, fixed, Errno::ENOMEM),
            (USER_END, P, RW, fixed, Errno::ENOMEM),
            (0x40_0001, P, RW, fixed, Errno::EINVAL),
            (MIN_USER_ADDR - P, P, RW, fixed, Errno::EPERM),
            (0, P, RW, fixed, Errno::EPERM),
        ];
        for (addr, len, prot, flags, errno) in cases {
            assert_eq!(m.mmap(&mut t, addr, len, prot, flags), Err(errno), "{addr:#x} {len:#x} {prot} {flags:#x}");
        }
        assert_eq!(m.areas().count(), 0, "a failed call maps nothing");
        assert_eq!(m.mmap(&mut t, 0, P, RW, MAP_SHARED | MAP_ANONYMOUS | 0x4_0000), Ok(MMAP_TOP - P), "shared and unknown flags");
        assert_eq!(m.mmap(&mut t, USER_END - P, P, RW, fixed), Ok(USER_END - P), "fixed may go above MMAP_TOP");
    }

    #[test]
    fn hints_like_go_uses_them() {
        let (mut m, mut t) = (Mappings::new(), Fake::default());
        let hint = 0xc0_0000_0000;
        assert_eq!(m.mmap(&mut t, hint, 64 * MIB, 0, ANON), Ok(hint), "a free hint is taken");
        let other = m.mmap(&mut t, hint, 64 * MIB, 0, ANON).unwrap();
        assert_ne!(other, hint, "a taken one is not");
        assert_eq!(m.mmap(&mut t, hint + 64 * MIB + 5, P, 0, ANON), Ok(hint + 64 * MIB), "rounded down");
        assert_eq!(m.mmap(&mut t, MMAP_TOP, P, 0, ANON).unwrap(), other - P, "not above MMAP_TOP");
        // Go's last resort: reserve more than needed and cut an aligned piece out of it
        let wide = m.mmap(&mut t, 0, 128 * MIB, 0, ANON).unwrap();
        let aligned = (wide + 64 * MIB - 1) & !(64 * MIB - 1);
        if aligned > wide {
            m.munmap(&mut t, wide, aligned - wide).unwrap();
        }
        m.munmap(&mut t, aligned + 64 * MIB, wide + 128 * MIB - aligned - 64 * MIB).unwrap();
        assert_eq!(m.find(aligned).map(|vma| (vma.start, vma.end)), Some((aligned, aligned + 64 * MIB)));
        assert!(m.find(aligned - 1).is_none_or(|vma| vma.end <= aligned));
        // and commit pieces of it
        assert_eq!(m.mmap(&mut t, aligned + 4 * MIB, 4 * MIB, RW, ANON | MAP_FIXED), Ok(aligned + 4 * MIB));
        assert!(t.store(&mut m, aligned + 4 * MIB, 1));
        assert!(!t.store(&mut m, aligned + 4 * MIB - 1, 1), "the reservation around stays PROT_NONE");
        // Linux-sized reservations: the whole 1.1 GiB of Go's start costs nothing here
        for _ in 0..2 {
            m.mmap(&mut t, 0, 512 * MIB, 0, ANON).unwrap();
        }
        assert_eq!(t.pages.len(), 1);
    }

    #[test]
    fn map_fixed_replaces_what_was_there() {
        let (mut m, mut t) = (Mappings::new(), Fake::default());
        let addr = m.mmap(&mut t, 0, 4 * P, RW, ANON).unwrap();
        for page in 0..4 {
            assert!(t.store(&mut m, addr + page * P, 7));
        }
        assert_eq!(m.mmap(&mut t, addr + P, 2 * P, RW, ANON | MAP_FIXED_NOREPLACE), Err(Errno::EEXIST));
        assert_eq!(t.load(&mut m, addr + P), Some(7), "NOREPLACE left it alone");
        assert_eq!(m.mmap(&mut t, addr + P, 2 * P, PROT_READ, ANON | MAP_FIXED), Ok(addr + P));
        assert_eq!(t.load(&mut m, addr + P), Some(0), "fresh zero pages");
        assert!(!t.store(&mut m, addr + 2 * P, 1), "with the new rights");
        assert_eq!(t.load(&mut m, addr), Some(7));
        assert_eq!(t.load(&mut m, addr + 3 * P), Some(7));
        let free = m.mmap(&mut t, 0, P, RW, ANON).unwrap();
        assert_eq!(m.mmap(&mut t, free - P, P, RW, ANON | MAP_FIXED | MAP_FIXED_NOREPLACE), Ok(free - P), "a free range");
    }

    #[test]
    fn munmap() {
        let (mut m, mut t) = (Mappings::new(), Fake::default());
        let addr = m.mmap(&mut t, 0, 3 * P, RW, ANON).unwrap();
        for page in 0..3 {
            assert!(t.store(&mut m, addr + page * P, 1));
        }
        assert_eq!(m.munmap(&mut t, addr + P, 1), Ok(()), "the length is rounded up");
        assert_eq!(t.load(&mut m, addr + P), None, "gone");
        assert_eq!(t.load(&mut m, addr), Some(1));
        assert_eq!(t.load(&mut m, addr + 2 * P), Some(1));
        assert_eq!(m.munmap(&mut t, addr + P, P), Ok(()), "twice is fine");
        assert_eq!(m.munmap(&mut t, 0x1000_0000, 1 << 40), Ok(()), "nothing there");
        for (addr, len) in [(addr + 1, P), (addr, 0), (addr, u64::MAX), (USER_END, P), (USER_END - P, 2 * P)] {
            assert_eq!(m.munmap(&mut t, addr, len), Err(Errno::EINVAL), "{addr:#x} {len:#x}");
        }
        assert_eq!(t.pages.len(), 2);
    }

    #[test]
    fn mprotect_keeps_the_data() {
        let (mut m, mut t) = (Mappings::new(), Fake::default());
        let addr = m.mmap(&mut t, 0, 2 * P, RW, ANON).unwrap();
        assert!(t.store(&mut m, addr, 9));
        m.mprotect(&mut t, addr, P, PROT_READ).unwrap();
        assert!(!t.store(&mut m, addr, 1), "read only now");
        assert_eq!(t.load(&mut m, addr), Some(9));
        assert!(t.store(&mut m, addr + P, 1), "the other page kept its rights");
        m.mprotect(&mut t, addr, P, 0).unwrap();
        assert_eq!(t.pages[&addr].0, PROT_NONE_FLAGS, "the frame stays, not present");
        assert_eq!(t.load(&mut m, addr), None);
        m.mprotect(&mut t, addr, P, RW).unwrap();
        assert_eq!(t.load(&mut m, addr), Some(9), "the data came back");
        assert!(t.store(&mut m, addr, 2));
        assert_eq!(m.mprotect(&mut t, addr, P, 8), Err(Errno::EINVAL), "bad rights");
        assert_eq!(m.mprotect(&mut t, addr, 0, 8), Ok(()), "nothing to do comes first");
        assert_eq!(m.mprotect(&mut t, addr + 1, P, RW), Err(Errno::EINVAL));
        assert_eq!(m.mprotect(&mut t, 0x1000_0000, 0, RW), Ok(()), "nothing to do");
        assert_eq!(m.mprotect(&mut t, addr, u64::MAX, RW), Err(Errno::ENOMEM));
        assert_eq!(m.mprotect(&mut t, USER_END - P, 2 * P, RW), Err(Errno::ENOMEM));
    }

    #[test]
    fn mprotect_over_a_hole_changes_nothing() {
        let (mut m, mut t) = (Mappings::new(), Fake::default());
        let addr = m.mmap(&mut t, 0, 3 * P, RW, ANON).unwrap();
        assert!(t.store(&mut m, addr, 1));
        m.munmap(&mut t, addr + P, P).unwrap();
        let areas: Vec<Vma> = m.areas().copied().collect();
        let flushes = t.flushes;
        assert_eq!(m.mprotect(&mut t, addr, 3 * P, PROT_READ), Err(Errno::ENOMEM));
        assert_eq!(m.areas().copied().collect::<Vec<_>>(), areas);
        assert_eq!(t.flushes, flushes, "no page touched");
        assert!(t.store(&mut m, addr, 2));
    }

    #[test]
    fn madvise_dontneed_gives_zeros() {
        let (mut m, mut t) = (Mappings::new(), Fake::default());
        let addr = m.mmap(&mut t, 0, 2 * P, RW, ANON).unwrap();
        assert!(t.store(&mut m, addr, 5) && t.store(&mut m, addr + P, 6));
        assert_eq!(m.madvise(&mut t, addr, 2 * P, MADV_NOHUGEPAGE), Ok(()));
        assert_eq!(t.load(&mut m, addr), Some(5), "other advice keeps the data");
        assert_eq!(m.madvise(&mut t, addr, P, MADV_DONTNEED), Ok(()));
        assert_eq!(t.load(&mut m, addr), Some(0));
        assert_eq!(t.load(&mut m, addr + P), Some(6));
        assert!(t.store(&mut m, addr, 5));
        assert_eq!(m.madvise(&mut t, addr, 3 * P, MADV_FREE), Err(Errno::ENOMEM), "a hole after the area");
        assert_eq!(t.load(&mut m, addr + P), Some(0), "the mapped part was handled");
        assert_eq!(m.madvise(&mut t, addr, 0, MADV_DONTNEED), Ok(()));
        assert_eq!(m.madvise(&mut t, addr, P, 5), Err(Errno::EINVAL), "no such advice");
        assert_eq!(m.madvise(&mut t, addr, P, 200), Err(Errno::EINVAL));
        assert_eq!(m.madvise(&mut t, addr, P, 9), Err(Errno::EINVAL), "MADV_REMOVE needs shared memory");
        assert_eq!(m.madvise(&mut t, addr + 1, P, 0), Err(Errno::EINVAL));
        assert_eq!(m.madvise(&mut t, addr, u64::MAX, 0), Err(Errno::EINVAL));
        assert_eq!(m.madvise(&mut t, USER_END - P, 2 * P, MADV_DONTNEED), Err(Errno::ENOMEM));
    }

    #[test]
    fn kernel_copies_fault_pages_in() {
        let (mut m, mut t) = (Mappings::new(), Fake::default());
        let addr = m.mmap(&mut t, 0, 4 * P, RW, ANON).unwrap();
        let read = m.mmap(&mut t, addr - P, P, PROT_READ, ANON | MAP_FIXED).unwrap();
        m.fault_in(&mut t, addr + 10, 2 * P, Touch::Write);
        assert_eq!(t.pages.keys().copied().collect::<Vec<_>>(), [addr, addr + P, addr + 2 * P], "every page the range touches");
        m.fault_in(&mut t, read, 3 * P, Touch::Write);
        assert_eq!(t.pages.len(), 3, "stops at the read-only page");
        m.fault_in(&mut t, read, 2 * P, Touch::Read);
        assert_eq!(t.pages.len(), 4);
        m.fault_in(&mut t, addr + 3 * P, u64::MAX, Touch::Write);
        assert_eq!(t.pages.len(), 5, "stops after the area");
    }

    /// Random calls against a model of every page: rights and data
    #[test]
    fn random_calls_match_a_model() {
        const BASE: u64 = 0x100_0000;
        const PAGES: u64 = 32;
        let (mut m, mut t) = (Mappings::new(), Fake::default());
        // page → (rights, data) for mapped pages
        let mut model: BTreeMap<u64, (u32, u8)> = BTreeMap::new();
        let mut seed = 0x5eed_1234_u64;
        let mut random = move |limit: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % limit
        };
        let prots = [0, PROT_READ, RW, PROT_READ | PROT_EXEC];
        for _ in 0..20_000 {
            let first = random(PAGES);
            let count = (1 + random(6)).min(PAGES - first);
            let (start, len) = (BASE + first * P, count * P);
            let pages = first..first + count;
            let prot = prots[random(4) as usize];
            match random(8) {
                0 => {
                    m.mmap(&mut t, start, len, prot, ANON | MAP_FIXED).unwrap();
                    for page in pages {
                        model.insert(page, (prot, 0));
                    }
                }
                1 => {
                    m.munmap(&mut t, start, len).unwrap();
                    for page in pages {
                        model.remove(&page);
                    }
                }
                2 => {
                    let covered = pages.clone().all(|page| model.contains_key(&page));
                    assert_eq!(m.mprotect(&mut t, start, len, prot).is_ok(), covered);
                    if covered {
                        for page in pages {
                            model.get_mut(&page).unwrap().0 = prot;
                        }
                    }
                }
                3 => {
                    let covered = pages.clone().all(|page| model.contains_key(&page));
                    assert_eq!(m.madvise(&mut t, start, len, MADV_DONTNEED).is_ok(), covered);
                    for page in pages {
                        if let Some(page) = model.get_mut(&page) {
                            page.1 = 0;
                        }
                    }
                }
                6 => {
                    // pages the kernel maps right away, like program segments
                    m.munmap(&mut t, start, len).unwrap();
                    for page in pages.clone() {
                        t.pages.insert(BASE + page * P, (flags_of(prot), 0));
                    }
                    m.record(start, start + len, flags_of(prot));
                    for page in pages {
                        model.insert(page, (prot, 0));
                    }
                }
                7 => {
                    let allowed = model.get(&first).is_some_and(|(prot, _)| prot & PROT_EXEC != 0);
                    assert_eq!(t.fetch(&mut m, BASE + first * P), allowed, "fetch from page {first}");
                }
                4 => {
                    let value = random(255) as u8 + 1;
                    let page = first;
                    let allowed = model.get(&page).is_some_and(|(prot, _)| prot & PROT_WRITE != 0);
                    assert_eq!(t.store(&mut m, BASE + page * P, value), allowed, "store to page {page}");
                    if allowed {
                        model.get_mut(&page).unwrap().1 = value;
                    }
                }
                _ => {
                    let page = first;
                    let expected = model.get(&page).filter(|(prot, _)| *prot != 0).map(|(_, data)| *data);
                    assert_eq!(t.load(&mut m, BASE + page * P), expected, "load from page {page}");
                }
            }
            for (&page, &(flags, _)) in &t.pages {
                let (prot, _) = model[&((page - BASE) / P)];
                assert_eq!(flags, flags_of(prot), "a page has the rights of its area");
            }
        }
    }
}
