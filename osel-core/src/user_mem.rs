//! Checks of memory a program hands to the kernel
//!
//! A system call gets addresses from the program. Before the kernel reads or writes
//! there, every page of the range must belong to the program: lower half, present,
//! user accessible and, for writes, writable.

use crate::errno::{Errno, KResult};
use crate::memory::{PAGE_SIZE, USER_END};
use x86_64::structures::paging::PageTableFlags;

/// Whether the kernel reads the range or writes it
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

/// Checks `len` bytes at `start` for `access`. `page_flags` gives the flags of the page
/// at an address, `None` if it is not mapped. Anything wrong is `EFAULT`, like Linux.
/// An empty range is fine anywhere in the lower half.
pub fn check(start: u64, len: u64, access: Access, mut page_flags: impl FnMut(u64) -> Option<PageTableFlags>) -> KResult<()> {
    let end = start.checked_add(len).ok_or(Errno::EFAULT)?;
    if end > USER_END {
        return Err(Errno::EFAULT);
    }
    if len == 0 {
        return Ok(());
    }
    let mut needed = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
    if access == Access::Write {
        needed |= PageTableFlags::WRITABLE;
    }
    let mut page = start & !(PAGE_SIZE - 1);
    while page < end {
        if !page_flags(page).is_some_and(|flags| flags.contains(needed)) {
            return Err(Errno::EFAULT);
        }
        page += PAGE_SIZE;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;

    const R: PageTableFlags = PageTableFlags::PRESENT.union(PageTableFlags::USER_ACCESSIBLE);
    const RW: PageTableFlags = R.union(PageTableFlags::WRITABLE);
    const BASE: u64 = 0x40_0000;

    fn lookup(pages: &BTreeMap<u64, PageTableFlags>) -> impl FnMut(u64) -> Option<PageTableFlags> + '_ {
        |page| pages.get(&page).copied()
    }

    #[test]
    fn out_of_the_lower_half() {
        let pages = BTreeMap::new();
        for (start, len) in [(USER_END, 1), (USER_END - 1, 2), (u64::MAX, 1), (u64::MAX - 10, 100), (0xffff_8000_0000_0000, 0)] {
            assert_eq!(check(start, len, Access::Read, lookup(&pages)), Err(Errno::EFAULT), "{start:#x} {len}");
        }
        assert_eq!(check(0, 0, Access::Write, lookup(&pages)), Ok(()), "an empty range at NULL");
        assert_eq!(check(BASE, 1, Access::Read, lookup(&pages)), Err(Errno::EFAULT), "not mapped");
    }

    #[test]
    fn kernel_only_pages_are_refused() {
        let pages = BTreeMap::from([(BASE, PageTableFlags::PRESENT | PageTableFlags::WRITABLE)]);
        assert_eq!(check(BASE, 8, Access::Read, lookup(&pages)), Err(Errno::EFAULT));
    }

    /// Nikka 4-process-3 `stress`: ranges of 1–4 pages starting and ending around
    /// quarter page boundaries, with a hole that is read-only or not mapped
    #[test]
    fn every_page_of_the_range_is_checked() {
        let quarter = PAGE_SIZE / 4;
        for pages in 1..=4u64 {
            for hole in 0..pages {
                for read_only in [false, true] {
                    let mut map = BTreeMap::new();
                    for page in 0..pages {
                        if page != hole {
                            map.insert(BASE + page * PAGE_SIZE, RW);
                        } else if read_only {
                            map.insert(BASE + page * PAGE_SIZE, R);
                        }
                    }
                    let hole_start = BASE + hole * PAGE_SIZE;
                    let hole_end = hole_start + PAGE_SIZE;
                    for start_quarter in 0..4 * pages {
                        for end_quarter in start_quarter..=4 * pages {
                            for start_shift in [-2i64, 0, 2] {
                                for end_shift in [-2i64, 0, 2] {
                                    let start = (BASE + start_quarter * quarter).saturating_add_signed(start_shift).max(BASE);
                                    let end = (BASE + end_quarter * quarter).saturating_add_signed(end_shift).min(BASE + pages * PAGE_SIZE);
                                    if end < start {
                                        continue;
                                    }
                                    let len = end - start;
                                    let touches_hole = len > 0 && start < hole_end && hole_start < end;
                                    let readable = !touches_hole || read_only;
                                    let writable = !touches_hole;
                                    let read = check(start, len, Access::Read, lookup(&map)).is_ok();
                                    let write = check(start, len, Access::Write, lookup(&map)).is_ok();
                                    assert_eq!(read, readable, "read {start:#x}..{end:#x} hole {hole} ro {read_only}");
                                    assert_eq!(write, writable, "write {start:#x}..{end:#x} hole {hole} ro {read_only}");
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
