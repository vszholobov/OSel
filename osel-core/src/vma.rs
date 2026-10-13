//! The areas of a program's address space: what `mmap` gave it, with which rights
//!
//! Pages of an area are mapped lazily, on the first page fault; the page tables only show
//! what was touched. Areas never overlap; neighbours with the same rights are merged, so
//! Go's many adjacent `mmap` calls stay a few areas.

use crate::memory::PAGE_SIZE;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

pub const PROT_READ: u32 = 1;
pub const PROT_WRITE: u32 = 2;
pub const PROT_EXEC: u32 = 4;

/// One area: whole pages `start..end` with `PROT_*` rights
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vma {
    pub start: u64,
    pub end: u64,
    pub prot: u32,
}

/// The areas of one address space between `low` and `high`
#[derive(Debug, Clone)]
pub struct VmaSet {
    /// By start address
    areas: BTreeMap<u64, Vma>,
    low: u64,
    high: u64,
}

impl VmaSet {
    /// Areas may lie in `low..high`; new ones without a hint are placed as high as possible
    pub fn new(low: u64, high: u64) -> VmaSet {
        assert!(low % PAGE_SIZE == 0 && high % PAGE_SIZE == 0 && low < high);
        VmaSet { areas: BTreeMap::new(), low, high }
    }

    /// The area holding `address`
    pub fn find(&self, address: u64) -> Option<Vma> {
        self.areas.range(..=address).next_back().map(|(_, vma)| *vma).filter(|vma| address < vma.end)
    }

    /// Whether `start..end` is inside the allowed range
    pub fn in_range(&self, start: u64, end: u64) -> bool {
        self.low <= start && start < end && end <= self.high
    }

    /// Whether nothing is mapped anywhere in `start..end`
    pub fn is_free(&self, start: u64, end: u64) -> bool {
        self.areas.values().all(|vma| vma.end <= start || end <= vma.start)
    }

    /// The highest free `len` bytes below `high`, or `hint` if it is free and in range
    pub fn find_free(&self, len: u64, hint: Option<u64>) -> Option<u64> {
        if let Some(hint) = hint.filter(|hint| hint % PAGE_SIZE == 0) {
            if hint.checked_add(len).is_some_and(|end| self.in_range(hint, end) && self.is_free(hint, end)) {
                return Some(hint);
            }
        }
        // gaps from the top down
        let mut top = self.high;
        for vma in self.areas.values().rev() {
            if vma.end <= top && top - vma.end >= len {
                return Some(top - len);
            }
            top = top.min(vma.start);
        }
        (top >= self.low && top - self.low >= len).then(|| top - len)
    }

    /// Makes `start..end` one area with `prot`, cutting whatever was there (`MAP_FIXED`).
    /// Returns the ranges that were mapped before: their pages must go.
    pub fn insert(&mut self, start: u64, end: u64, prot: u32) -> Vec<(u64, u64)> {
        let removed = self.remove(start, end);
        self.areas.insert(start, Vma { start, end, prot });
        self.merge_around(start);
        removed
    }

    /// Removes `start..end` from the areas, cutting the ones it crosses. Returns the ranges
    /// that were mapped.
    pub fn remove(&mut self, start: u64, end: u64) -> Vec<(u64, u64)> {
        let crossing: Vec<Vma> = self.areas.values().filter(|vma| vma.start < end && start < vma.end).copied().collect();
        let mut removed = Vec::new();
        for vma in crossing {
            self.areas.remove(&vma.start);
            if vma.start < start {
                self.areas.insert(vma.start, Vma { end: start, ..vma });
            }
            if end < vma.end {
                self.areas.insert(end, Vma { start: end, ..vma });
            }
            removed.push((vma.start.max(start), vma.end.min(end)));
        }
        removed
    }

    /// Gives `start..end` the rights `prot`. `None`, and nothing changes, if a part of the
    /// range is not mapped (`ENOMEM` for `mprotect`).
    pub fn protect(&mut self, start: u64, end: u64, prot: u32) -> Option<()> {
        if !self.covered(start, end) {
            return None;
        }
        self.insert(start, end, prot);
        Some(())
    }

    /// Whether every byte of `start..end` is in an area
    pub fn covered(&self, start: u64, end: u64) -> bool {
        let mut at = start;
        while at < end {
            match self.find(at) {
                Some(vma) => at = vma.end,
                None => return false,
            }
        }
        true
    }

    /// Joins the area at `start` with neighbours that have the same rights
    fn merge_around(&mut self, start: u64) {
        let mut vma = self.areas[&start];
        if let Some(prev) = self.areas.range(..start).next_back().map(|(_, prev)| *prev) {
            if prev.end == vma.start && prev.prot == vma.prot {
                self.areas.remove(&vma.start);
                vma = Vma { start: prev.start, ..vma };
                self.areas.insert(prev.start, vma);
            }
        }
        if let Some(next) = self.areas.get(&vma.end).copied() {
            if next.prot == vma.prot {
                self.areas.remove(&next.start);
                self.areas.insert(vma.start, Vma { end: next.end, ..vma });
            }
        }
    }

    pub fn areas(&self) -> impl Iterator<Item = &Vma> {
        self.areas.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: u64 = PAGE_SIZE;
    const RW: u32 = PROT_READ | PROT_WRITE;

    fn set() -> VmaSet {
        VmaSet::new(16 * P, 64 * P)
    }

    #[test]
    fn insert_find_and_merge() {
        let mut set = set();
        assert!(set.insert(20 * P, 22 * P, RW).is_empty());
        assert!(set.insert(22 * P, 24 * P, RW).is_empty());
        assert_eq!(set.areas().count(), 1, "neighbours with the same rights merge");
        assert_eq!(set.find(23 * P + 5), Some(Vma { start: 20 * P, end: 24 * P, prot: RW }));
        assert_eq!(set.find(24 * P), None);
        assert_eq!(set.find(19 * P), None);
        set.insert(24 * P, 25 * P, PROT_READ);
        assert_eq!(set.areas().count(), 2, "different rights stay apart");
    }

    #[test]
    fn map_fixed_cuts_what_was_there() {
        let mut set = set();
        set.insert(20 * P, 30 * P, PROT_READ);
        let removed = set.insert(22 * P, 24 * P, RW);
        assert_eq!(removed, [(22 * P, 24 * P)]);
        let areas: Vec<Vma> = set.areas().copied().collect();
        assert_eq!(areas, [
            Vma { start: 20 * P, end: 22 * P, prot: PROT_READ },
            Vma { start: 22 * P, end: 24 * P, prot: RW },
            Vma { start: 24 * P, end: 30 * P, prot: PROT_READ },
        ]);
    }

    #[test]
    fn remove_cuts_and_reports() {
        let mut set = set();
        set.insert(20 * P, 24 * P, RW);
        set.insert(26 * P, 30 * P, PROT_READ);
        let removed = set.remove(22 * P, 28 * P);
        assert_eq!(removed, [(22 * P, 24 * P), (26 * P, 28 * P)]);
        assert!(set.find(23 * P).is_none() && set.find(27 * P).is_none());
        assert!(set.find(21 * P).is_some() && set.find(29 * P).is_some());
        assert!(set.remove(40 * P, 50 * P).is_empty(), "nothing there is fine");
    }

    #[test]
    fn protect_needs_the_whole_range() {
        let mut set = set();
        set.insert(20 * P, 22 * P, RW);
        set.insert(23 * P, 25 * P, RW);
        assert_eq!(set.protect(20 * P, 25 * P, PROT_READ), None, "a hole in the middle");
        assert_eq!(set.find(20 * P).unwrap().prot, RW, "nothing changed");
        assert_eq!(set.protect(21 * P, 22 * P, PROT_READ), Some(()));
        assert_eq!(set.find(21 * P).unwrap().prot, PROT_READ);
        assert_eq!(set.find(20 * P).unwrap().prot, RW);
    }

    #[test]
    fn free_space_from_the_top() {
        let mut set = set();
        assert_eq!(set.find_free(4 * P, None), Some(60 * P));
        set.insert(60 * P, 64 * P, RW);
        assert_eq!(set.find_free(4 * P, None), Some(56 * P));
        assert_eq!(set.find_free(4 * P, Some(30 * P)), Some(30 * P), "a free hint is taken");
        assert_eq!(set.find_free(4 * P, Some(62 * P)), Some(56 * P), "a taken hint is not");
        assert_eq!(set.find_free(4 * P, Some(30 * P + 1)), Some(56 * P), "an unaligned hint is not");
        assert_eq!(set.find_free(4 * P, Some(u64::MAX - P + 1)), Some(56 * P), "no overflow");
        assert_eq!(set.find_free(48 * P, None), None, "too big");
        assert_eq!(set.find_free(44 * P, None), Some(16 * P), "exactly what is left");
        set.insert(30 * P, 31 * P, RW);
        assert_eq!(set.find_free(20 * P, None), Some(40 * P), "below the top area, above the middle one");
        assert_eq!(set.find_free(30 * P, None), None, "no gap of 30 pages");
        assert_eq!(set.find_free(14 * P, None), Some(46 * P), "the highest gap that fits");
        assert_eq!(set.find_free(29 * P, None), Some(31 * P), "the gap between two areas");
    }

    /// Random operations against a model with the rights of every page
    #[test]
    fn random_operations_match_a_model() {
        const PAGES: u64 = 48;
        let mut set = set();
        let mut model: [Option<u32>; PAGES as usize] = [None; PAGES as usize];
        let mut seed = 0x0f0e_d0c0_u64;
        let mut random = move |limit: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % limit
        };
        for _ in 0..20_000 {
            let first = random(PAGES);
            let last = (first + random(8)).min(PAGES - 1);
            let (start, end) = ((16 + first) * P, (17 + last) * P);
            let prot = random(3) as u32 + 1;
            match random(3) {
                0 => {
                    let was: Vec<bool> = (first..=last).map(|page| model[page as usize].is_some()).collect();
                    let removed = set.insert(start, end, prot);
                    let removed_pages: u64 = removed.iter().map(|(s, e)| (e - s) / P).sum();
                    assert_eq!(removed_pages, was.iter().filter(|&&m| m).count() as u64);
                    for page in first..=last {
                        model[page as usize] = Some(prot);
                    }
                }
                1 => {
                    set.remove(start, end);
                    for page in first..=last {
                        model[page as usize] = None;
                    }
                }
                _ => {
                    let covered = (first..=last).all(|page| model[page as usize].is_some());
                    assert_eq!(set.protect(start, end, prot).is_some(), covered);
                    if covered {
                        for page in first..=last {
                            model[page as usize] = Some(prot);
                        }
                    }
                }
            }
            for page in 0..PAGES {
                let found = set.find((16 + page) * P).map(|vma| vma.prot);
                assert_eq!(found, model[page as usize], "page {page}");
            }
            let areas: Vec<Vma> = set.areas().copied().collect();
            for pair in areas.windows(2) {
                assert!(pair[0].end <= pair[1].start, "overlap");
                assert!(pair[0].end < pair[1].start || pair[0].prot != pair[1].prot, "not merged");
            }
            if let Some(free) = set.find_free(3 * P, None) {
                assert!(set.is_free(free, free + 3 * P));
            }
        }
    }
}
