//! Loading static ELF64 programs for x86_64
//!
//! Only what a static Linux executable needs: `ET_EXEC`, `PT_LOAD` segments, no dynamic
//! linking. The loader doesn't touch page tables itself, it asks a `ProgramMemory` to map
//! zeroed pages and write bytes, so it runs on the host as a whole.

use crate::errno::{Errno, KResult};
use crate::memory::{PAGE_SIZE, USER_END};
use crate::vfs::Inode;
use alloc::vec;
use alloc::vec::Vec;
use x86_64::structures::paging::PageTableFlags;

/// Lowest address a segment may use: page 0 and its neighbours stay unmapped, so a null
/// pointer faults (Linux `vm.mmap_min_addr`)
pub const MIN_USER_ADDR: u64 = 0x10000;

/// Most memory the segments of one program may take: a broken `p_memsz` must not eat
/// every free frame
pub const MAX_PROGRAM_BYTES: u64 = 256 * 1024 * 1024;

/// Size of a program header of ELF64
pub const PHENT_SIZE: u16 = 56;

const HEADER_SIZE: usize = 64;
const MAX_PROGRAM_HEADERS: u16 = 64;

const ET_EXEC: u16 = 2;
const EM_X86_64: u16 = 62;
pub const PT_LOAD: u32 = 1;
pub const PT_DYNAMIC: u32 = 2;
pub const PT_INTERP: u32 = 3;
pub const PT_PHDR: u32 = 6;
pub const PT_TLS: u32 = 7;
pub const PF_X: u32 = 1;
pub const PF_W: u32 = 2;
pub const PF_R: u32 = 4;

/// A file the loader reads from
pub trait ReadAt {
    /// Size of the file in bytes
    fn size(&self) -> u64;

    /// Reads at `offset` into `buf`, returns how many bytes were read, 0 at the end
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> KResult<usize>;

    /// Fills `buf` from `offset`; the file ending early is `ENOEXEC`
    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> KResult<()> {
        let mut done = 0;
        while done < buf.len() {
            match self.read_at(offset + done as u64, &mut buf[done..])? {
                0 => return Err(Errno::ENOEXEC),
                read => done += read,
            }
        }
        Ok(())
    }
}

impl ReadAt for [u8] {
    fn size(&self) -> u64 {
        self.len() as u64
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> KResult<usize> {
        let start = usize::try_from(offset).unwrap_or(usize::MAX).min(self.len());
        let count = buf.len().min(self.len() - start);
        buf[..count].copy_from_slice(&self[start..start + count]);
        Ok(count)
    }
}

impl ReadAt for dyn Inode {
    fn size(&self) -> u64 {
        self.stat().size
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> KResult<usize> {
        Inode::read_at(self, offset, buf)
    }
}

/// Where a program is loaded: the loader maps zeroed pages, then writes the file bytes
pub trait ProgramMemory {
    /// Maps `size` bytes of zeroed pages at `start` with `flags`
    fn map_zeroed(&mut self, start: u64, size: u64, flags: PageTableFlags) -> KResult<()>;

    /// Writes `bytes` at `address`, mapped by `map_zeroed` before, whatever its flags
    fn write(&mut self, address: u64, bytes: &[u8]) -> KResult<()>;
}

/// A `PT_LOAD` segment
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub vaddr: u64,
    pub memsz: u64,
    pub offset: u64,
    pub filesz: u64,
    pub flags: u32,
}

impl Segment {
    fn end(&self) -> u64 {
        self.vaddr + self.memsz
    }

    fn page_flags(&self) -> PageTableFlags {
        let mut flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
        if self.flags & PF_W != 0 {
            flags |= PageTableFlags::WRITABLE;
        }
        if self.flags & PF_X == 0 {
            flags |= PageTableFlags::NO_EXECUTE;
        }
        flags
    }
}

/// What the loader found in a valid executable
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub entry: u64,
    /// Address of the program headers in memory, for `AT_PHDR`, if a segment loads them
    pub phdr: Option<u64>,
    pub phnum: u16,
    /// Non-empty `PT_LOAD` segments sorted by address
    pub segments: Vec<Segment>,
}

/// Pages mapped with the same flags
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageRun {
    pub start: u64,
    pub pages: u64,
    pub flags: PageTableFlags,
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap())
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

/// Reads and checks the headers of a static x86_64 executable. Anything else, and every
/// inconsistency, is `ENOEXEC`.
pub fn parse(file: &(impl ReadAt + ?Sized)) -> KResult<Program> {
    let mut header = [0u8; HEADER_SIZE];
    file.read_exact_at(0, &mut header)?;
    let valid = header[..4] == *b"\x7fELF"
        && header[4] == 2 // 64 bit
        && header[5] == 1 // little endian
        && header[6] == 1 // version
        && u16_at(&header, 16) == ET_EXEC
        && u16_at(&header, 18) == EM_X86_64
        && u16_at(&header, 54) == PHENT_SIZE;
    let entry = u64_at(&header, 24);
    let phoff = u64_at(&header, 32);
    let phnum = u16_at(&header, 56);
    if !valid || phnum == 0 || phnum > MAX_PROGRAM_HEADERS {
        return Err(Errno::ENOEXEC);
    }
    let table_size = phnum as u64 * PHENT_SIZE as u64;
    if phoff.checked_add(table_size).is_none_or(|end| end > file.size()) {
        return Err(Errno::ENOEXEC);
    }
    let mut table = vec![0u8; table_size as usize];
    file.read_exact_at(phoff, &mut table)?;

    let mut segments = Vec::new();
    for header in table.chunks_exact(PHENT_SIZE as usize) {
        let segment = Segment {
            flags: u32_at(header, 4),
            offset: u64_at(header, 8),
            vaddr: u64_at(header, 16),
            filesz: u64_at(header, 32),
            memsz: u64_at(header, 40),
        };
        match u32_at(header, 0) {
            PT_LOAD if segment.memsz > 0 => segments.push(check_segment(segment, file.size())?),
            // dynamic linking is not supported
            PT_INTERP | PT_DYNAMIC => return Err(Errno::ENOEXEC),
            // PT_TLS (Go sets up its TLS itself), PT_PHDR, PT_NOTE, PT_GNU_STACK, ...
            _ => {}
        }
    }
    if segments.is_empty() || segments.windows(2).any(|pair| pair[0].end() > pair[1].vaddr) {
        return Err(Errno::ENOEXEC);
    }
    if !segments.iter().any(|s| s.flags & PF_X != 0 && (s.vaddr..s.end()).contains(&entry)) {
        return Err(Errno::ENOEXEC);
    }
    // like Linux: the headers are visible if a segment loads their file bytes
    let phdr = segments
        .iter()
        .find(|s| s.offset <= phoff && phoff + table_size <= s.offset + s.filesz)
        .map(|s| s.vaddr + (phoff - s.offset));
    Ok(Program { entry, phdr, phnum, segments })
}

fn check_segment(segment: Segment, file_size: u64) -> KResult<Segment> {
    let in_file = segment.offset.checked_add(segment.filesz).is_some_and(|end| end <= file_size);
    let in_user_half = segment.vaddr >= MIN_USER_ADDR && segment.vaddr.checked_add(segment.memsz).is_some_and(|end| end <= USER_END);
    let writable_code = segment.flags & PF_W != 0 && segment.flags & PF_X != 0;
    if segment.filesz > segment.memsz || !in_file || !in_user_half || writable_code {
        return Err(Errno::ENOEXEC);
    }
    Ok(segment)
}

/// Pages to map for `segments` (sorted, not overlapping): a page that two segments share
/// gets the flags of both. Writable code, even on a shared page, is `ENOEXEC`.
pub fn page_plan(segments: &[Segment]) -> KResult<Vec<PageRun>> {
    let mut plan: Vec<PageRun> = Vec::new();
    for segment in segments {
        let first = segment.vaddr & !(PAGE_SIZE - 1);
        let last = (segment.end() - 1) & !(PAGE_SIZE - 1);
        let flags = segment.page_flags();
        let mut next = first;
        if let Some(run) = plan.last_mut() {
            let run_last = run.start + (run.pages - 1) * PAGE_SIZE;
            if run_last == first {
                // the shared page becomes a run of its own with both sets of flags
                let shared = (run.flags | flags) & !PageTableFlags::NO_EXECUTE | (run.flags & flags & PageTableFlags::NO_EXECUTE);
                run.pages -= 1;
                if run.pages == 0 {
                    plan.pop();
                }
                plan.push(PageRun { start: first, pages: 1, flags: shared });
                next = first + PAGE_SIZE;
            }
        }
        if next <= last {
            plan.push(PageRun { start: next, pages: (last - next) / PAGE_SIZE + 1, flags });
        }
    }
    let writable_code = |flags: PageTableFlags| flags.contains(PageTableFlags::WRITABLE) && !flags.contains(PageTableFlags::NO_EXECUTE);
    if plan.iter().any(|run| writable_code(run.flags)) {
        return Err(Errno::ENOEXEC);
    }
    Ok(plan)
}

/// Loads the executable `file` into `memory`: maps zeroed pages for every segment and
/// copies its file bytes, the rest of the segment (`.bss`) stays zero. Segments must end
/// at or below `end`, all together take at most `MAX_PROGRAM_BYTES` (`ENOMEM`). On error
/// the memory is half filled: the caller drops it.
pub fn load(file: &(impl ReadAt + ?Sized), memory: &mut impl ProgramMemory, end: u64) -> KResult<Program> {
    let program = parse(file)?;
    if program.segments.iter().any(|segment| segment.end() > end) {
        return Err(Errno::ENOEXEC);
    }
    let plan = page_plan(&program.segments)?;
    if plan.iter().map(|run| run.pages * PAGE_SIZE).sum::<u64>() > MAX_PROGRAM_BYTES {
        return Err(Errno::ENOMEM);
    }
    for run in plan {
        memory.map_zeroed(run.start, run.pages * PAGE_SIZE, run.flags)?;
    }
    let mut buffer = vec![0u8; PAGE_SIZE as usize];
    for segment in &program.segments {
        let mut done = 0;
        while done < segment.filesz {
            let chunk = (segment.filesz - done).min(PAGE_SIZE) as usize;
            file.read_exact_at(segment.offset + done, &mut buffer[..chunk])?;
            memory.write(segment.vaddr + done, &buffer[..chunk])?;
            done += chunk as u64;
        }
    }
    Ok(program)
}

/// Writes ELF executables for tests: headers first, then each segment's bytes at a file
/// offset congruent to its address modulo the page size, like real linkers do
pub struct ElfBuilder {
    entry: u64,
    headers: Vec<(u32, Segment)>,
    data: Vec<Vec<u8>>,
}

impl ElfBuilder {
    pub fn new(entry: u64) -> ElfBuilder {
        ElfBuilder { entry, headers: Vec::new(), data: Vec::new() }
    }

    /// A `PT_LOAD` segment with `bytes` from the file and `memsz` bytes in memory
    pub fn segment(mut self, vaddr: u64, flags: u32, bytes: &[u8], memsz: u64) -> ElfBuilder {
        let segment = Segment { vaddr, memsz, offset: 0, filesz: bytes.len() as u64, flags };
        self.headers.push((PT_LOAD, segment));
        self.data.push(bytes.to_vec());
        self
    }

    /// A program header written as is, without data
    pub fn header(mut self, kind: u32, segment: Segment) -> ElfBuilder {
        self.headers.push((kind, segment));
        self.data.push(Vec::new());
        self
    }

    pub fn build(&self) -> Vec<u8> {
        let table_end = HEADER_SIZE as u64 + self.headers.len() as u64 * PHENT_SIZE as u64;
        let mut file = vec![0u8; table_end as usize];
        file[..4].copy_from_slice(b"\x7fELF");
        file[4..7].copy_from_slice(&[2, 1, 1]);
        file[16..18].copy_from_slice(&ET_EXEC.to_le_bytes());
        file[18..20].copy_from_slice(&EM_X86_64.to_le_bytes());
        file[20..24].copy_from_slice(&1u32.to_le_bytes());
        file[24..32].copy_from_slice(&self.entry.to_le_bytes());
        file[32..40].copy_from_slice(&(HEADER_SIZE as u64).to_le_bytes());
        file[52..54].copy_from_slice(&(HEADER_SIZE as u16).to_le_bytes());
        file[54..56].copy_from_slice(&PHENT_SIZE.to_le_bytes());
        file[56..58].copy_from_slice(&(self.headers.len() as u16).to_le_bytes());
        for (index, ((kind, segment), data)) in self.headers.iter().zip(&self.data).enumerate() {
            let mut segment = *segment;
            if *kind == PT_LOAD {
                let page = (file.len() as u64).next_multiple_of(PAGE_SIZE);
                segment.offset = page + segment.vaddr % PAGE_SIZE;
                file.resize(segment.offset as usize, 0);
                file.extend_from_slice(data);
            }
            let at = HEADER_SIZE + index * PHENT_SIZE as usize;
            let header = &mut file[at..at + PHENT_SIZE as usize];
            header[0..4].copy_from_slice(&kind.to_le_bytes());
            header[4..8].copy_from_slice(&segment.flags.to_le_bytes());
            header[8..16].copy_from_slice(&segment.offset.to_le_bytes());
            header[16..24].copy_from_slice(&segment.vaddr.to_le_bytes());
            header[24..32].copy_from_slice(&segment.vaddr.to_le_bytes());
            header[32..40].copy_from_slice(&segment.filesz.to_le_bytes());
            header[40..48].copy_from_slice(&segment.memsz.to_le_bytes());
            header[48..56].copy_from_slice(&PAGE_SIZE.to_le_bytes());
        }
        file
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;

    const TEXT: u64 = 0x40_0000;
    const DATA: u64 = 0x60_0000;
    const R: PageTableFlags = PageTableFlags::PRESENT.union(PageTableFlags::USER_ACCESSIBLE);
    const RX: PageTableFlags = R;
    const RNX: PageTableFlags = R.union(PageTableFlags::NO_EXECUTE);
    const RW: PageTableFlags = RNX.union(PageTableFlags::WRITABLE);

    /// Pages as a map, prefilled with 0xff when mapped so missing zeroing shows up;
    /// writes outside mapped pages fail the test
    #[derive(Default)]
    struct FakeMemory {
        pages: BTreeMap<u64, (PageTableFlags, Vec<u8>)>,
        zero: bool,
    }

    impl FakeMemory {
        fn new() -> FakeMemory {
            FakeMemory { pages: BTreeMap::new(), zero: true }
        }

        fn read(&self, address: u64, len: usize) -> Vec<u8> {
            (address..address + len as u64)
                .map(|a| self.pages[&(a & !(PAGE_SIZE - 1))].1[(a % PAGE_SIZE) as usize])
                .collect()
        }

        fn flags(&self, address: u64) -> Option<PageTableFlags> {
            self.pages.get(&(address & !(PAGE_SIZE - 1))).map(|page| page.0)
        }
    }

    impl ProgramMemory for FakeMemory {
        fn map_zeroed(&mut self, start: u64, size: u64, flags: PageTableFlags) -> KResult<()> {
            assert_eq!(start % PAGE_SIZE, 0);
            assert_eq!(size % PAGE_SIZE, 0);
            // corrupted sizes can ask for terabytes, like a real space would run out of frames
            if self.pages.len() as u64 + size / PAGE_SIZE > 1024 {
                return Err(Errno::ENOMEM);
            }
            for page in (start..start + size).step_by(PAGE_SIZE as usize) {
                let fill = if self.zero { 0 } else { 0xff };
                let old = self.pages.insert(page, (flags, vec![fill; PAGE_SIZE as usize]));
                assert!(old.is_none(), "page {page:#x} mapped twice");
            }
            Ok(())
        }

        fn write(&mut self, address: u64, bytes: &[u8]) -> KResult<()> {
            for (i, &byte) in bytes.iter().enumerate() {
                let a = address + i as u64;
                let page = self.pages.get_mut(&(a & !(PAGE_SIZE - 1))).unwrap_or_else(|| panic!("write to unmapped {a:#x}"));
                page.1[(a % PAGE_SIZE) as usize] = byte;
            }
            Ok(())
        }
    }

    fn hello() -> ElfBuilder {
        ElfBuilder::new(TEXT + 0x10)
            .segment(TEXT, PF_R | PF_X, &[0x90; 0x30], 0x30)
            .segment(DATA, PF_R | PF_W, b"data", 0x2000)
    }

    fn segment(vaddr: u64, flags: u32, filesz: u64, memsz: u64) -> Segment {
        Segment { vaddr, memsz, offset: 0, filesz, flags }
    }

    #[test]
    fn a_valid_program_is_parsed() {
        let program = parse(&hello().build()[..]).unwrap();
        assert_eq!(program.entry, TEXT + 0x10);
        assert_eq!(program.phnum, 2);
        assert_eq!(program.segments.len(), 2);
        assert_eq!(program.segments[1].memsz, 0x2000);
        assert_eq!(program.phdr, None, "no segment loads the headers");
    }

    #[test]
    fn loading_maps_zeroed_pages_and_copies_the_file() {
        let mut memory = FakeMemory::new();
        let program = load(&hello().build()[..], &mut memory, USER_END).unwrap();
        assert_eq!(program.entry, TEXT + 0x10);
        assert_eq!(memory.pages.len(), 3, "a text page and two data pages");
        assert_eq!(memory.read(TEXT, 0x30), [0x90; 0x30]);
        assert_eq!(memory.read(TEXT + 0x30, 16), [0; 16], "the tail of the page is zero");
        assert_eq!(memory.read(DATA, 4), b"data");
        assert!(memory.read(DATA + 4, 0x2000 - 4).iter().all(|&b| b == 0), ".bss is zero");
        assert_eq!(memory.flags(TEXT), Some(RX));
        assert_eq!(memory.flags(DATA), Some(RW));
        assert_eq!(memory.flags(DATA + PAGE_SIZE), Some(RW));
    }

    #[test]
    fn headers_loaded_by_a_segment_give_at_phdr() {
        // like Go: the text segment starts at file offset 0 and holds the headers
        let mut file = ElfBuilder::new(TEXT + 0x1000).segment(TEXT + 0x1000, PF_R | PF_X, &[0xc3], 1).build();
        // let the text segment cover the file from offset 0
        let at = HEADER_SIZE;
        let size = file.len() as u64;
        file[at + 8..at + 16].copy_from_slice(&0u64.to_le_bytes());
        file[at + 16..at + 24].copy_from_slice(&TEXT.to_le_bytes());
        file[at + 32..at + 40].copy_from_slice(&size.to_le_bytes());
        file[at + 40..at + 48].copy_from_slice(&size.to_le_bytes());
        let program = parse(&file[..]).unwrap();
        assert_eq!(program.phdr, Some(TEXT + HEADER_SIZE as u64));
    }

    #[test]
    fn bad_headers_are_enoexec() {
        let good = hello().build();
        let corrupt = |at: usize, bytes: &[u8]| {
            let mut file = good.clone();
            file[at..at + bytes.len()].copy_from_slice(bytes);
            file
        };
        let cases = [
            ("magic", corrupt(1, b"X")),
            ("32 bit", corrupt(4, &[1])),
            ("big endian", corrupt(5, &[2])),
            ("version", corrupt(6, &[0])),
            ("shared object", corrupt(16, &3u16.to_le_bytes())),
            ("aarch64", corrupt(18, &183u16.to_le_bytes())),
            ("program header size", corrupt(54, &32u16.to_le_bytes())),
            ("no program headers", corrupt(56, &0u16.to_le_bytes())),
            ("too many program headers", corrupt(56, &65u16.to_le_bytes())),
            ("headers past the end", corrupt(32, &(good.len() as u64 - 8).to_le_bytes())),
            ("headers offset overflow", corrupt(32, &u64::MAX.to_le_bytes())),
            ("entry outside the code", corrupt(24, &(DATA + 1).to_le_bytes())),
            ("truncated", good[..40].to_vec()),
            ("empty", Vec::new()),
        ];
        for (name, file) in cases {
            assert_eq!(parse(&file[..]), Err(Errno::ENOEXEC), "{name}");
        }
    }

    #[test]
    fn bad_segments_are_enoexec() {
        let x = PF_R | PF_X;
        let w = PF_R | PF_W;
        let cases: [(&str, Segment); 7] = [
            ("filesz > memsz", segment(DATA, w, 0x20, 0x10)),
            ("address overflow", segment(u64::MAX - 0x10, w, 0, 0x100)),
            ("in the upper half", segment(0xffff_8000_0000_0000, w, 0, 0x100)),
            ("crossing into the hole", segment(USER_END - 0x10, w, 0, 0x100)),
            ("page zero", segment(0x1000, w, 0, 0x100)),
            ("writable code", segment(DATA, x | PF_W, 0, 0x100)),
            ("past the end of the file", segment(DATA, w, 0x10_0000, 0x10_0000)),
        ];
        for (name, bad) in cases {
            let file = ElfBuilder::new(TEXT).segment(TEXT, x, &[0xc3], 1).header(PT_LOAD, bad).build();
            assert_eq!(parse(&file[..]), Err(Errno::ENOEXEC), "{name}");
        }
        let overlap = ElfBuilder::new(TEXT).segment(TEXT, x, &[0xc3], 0x2000).segment(TEXT + 0x1000, w, b"", 0x10);
        assert_eq!(parse(&overlap.build()[..]), Err(Errno::ENOEXEC), "overlap");
        let unsorted = ElfBuilder::new(TEXT).segment(DATA, w, b"", 0x10).segment(TEXT, x, &[0xc3], 1);
        assert_eq!(parse(&unsorted.build()[..]), Err(Errno::ENOEXEC), "out of order");
        let dynamic = hello().header(PT_INTERP, segment(0, PF_R, 0, 0));
        assert_eq!(parse(&dynamic.build()[..]), Err(Errno::ENOEXEC), "interpreter");
        let file_offset_overflow = {
            let mut file = hello().build();
            let at = HEADER_SIZE + PHENT_SIZE as usize;
            file[at + 8..at + 16].copy_from_slice(&u64::MAX.to_le_bytes());
            file
        };
        assert_eq!(parse(&file_offset_overflow[..]), Err(Errno::ENOEXEC), "file offset overflow");
        assert_eq!(load(&hello().build()[..], &mut FakeMemory::new(), DATA + 0x1000), Err(Errno::ENOEXEC), "past the end limit");
        assert!(load(&hello().build()[..], &mut FakeMemory::new(), DATA + 0x2000).is_ok(), "right up to the limit");
        let huge = ElfBuilder::new(TEXT).segment(TEXT, PF_R | PF_X, &[0xc3], 1).segment(DATA, PF_R | PF_W, b"", MAX_PROGRAM_BYTES);
        assert_eq!(load(&huge.build()[..], &mut FakeMemory::new(), USER_END), Err(Errno::ENOMEM), "too big before mapping anything");
    }

    #[test]
    fn other_headers_are_ignored() {
        // Go has a PT_TLS at address 0 and a PT_GNU_STACK with nothing at all
        let program = hello().header(PT_TLS, segment(0, PF_R, 0, 8)).header(0x6474_e551, segment(0, PF_R | PF_W, 0, 0));
        assert!(parse(&program.build()[..]).is_ok());
        let empty_load = hello().header(PT_LOAD, segment(0, PF_R, 0, 0));
        assert_eq!(parse(&empty_load.build()[..]).unwrap().segments.len(), 2, "an empty PT_LOAD is skipped");
    }

    #[test]
    fn a_shared_page_gets_both_sets_of_flags() {
        let x = PF_R | PF_X;
        // code then read-only data on the same page: executable
        let plan = page_plan(&[segment(TEXT, x, 0, 0x1800), segment(TEXT + 0x1800, PF_R, 0, 0x1000)]).unwrap();
        assert_eq!(plan, [
            PageRun { start: TEXT, pages: 1, flags: RX },
            PageRun { start: TEXT + 0x1000, pages: 1, flags: RX },
            PageRun { start: TEXT + 0x2000, pages: 1, flags: RNX },
        ]);
        // read-only then writable data: writable
        let plan = page_plan(&[segment(DATA, PF_R, 0, 0x800), segment(DATA + 0x800, PF_R | PF_W, 0, 0x800)]).unwrap();
        assert_eq!(plan, [PageRun { start: DATA, pages: 1, flags: RW }]);
        // three segments on one page
        let plan = page_plan(&[segment(DATA, PF_R, 0, 0x10), segment(DATA + 0x10, PF_R, 0, 0x10), segment(DATA + 0x20, PF_R | PF_W, 0, 0x10)]).unwrap();
        assert_eq!(plan, [PageRun { start: DATA, pages: 1, flags: RW }]);
        // code and writable data on one page would be writable code
        assert_eq!(page_plan(&[segment(TEXT, x, 0, 0x10), segment(TEXT + 0x10, PF_R | PF_W, 0, 0x10)]), Err(Errno::ENOEXEC));
    }

    #[test]
    fn bytes_of_neighbour_segments_on_a_shared_page_survive() {
        let file = ElfBuilder::new(TEXT).segment(TEXT, PF_R | PF_X, b"code", 4).segment(TEXT + 4, PF_R, b"rodata", 0x20).build();
        let mut memory = FakeMemory::new();
        load(&file[..], &mut memory, USER_END).unwrap();
        assert_eq!(memory.read(TEXT, 10), b"coderodata");
        assert_eq!(memory.pages.len(), 1);
    }

    /// An inode that returns one byte per call: the loader must not rely on full reads
    #[test]
    fn short_reads_are_retried() {
        use crate::vfs::{FileType, Stat};
        struct Trickle(Vec<u8>);
        impl Inode for Trickle {
            fn stat(&self) -> Stat {
                Stat { ino: 1, file_type: FileType::Regular, permissions: 0o755, size: self.0.len() as u64 }
            }
            fn read_at(&self, offset: u64, buf: &mut [u8]) -> KResult<usize> {
                let offset = offset as usize;
                if offset >= self.0.len() || buf.is_empty() {
                    return Ok(0);
                }
                buf[0] = self.0[offset];
                Ok(1)
            }
        }
        let inode: alloc::boxed::Box<dyn Inode> = alloc::boxed::Box::new(Trickle(hello().build()));
        let mut memory = FakeMemory::new();
        load(&*inode, &mut memory, USER_END).unwrap();
        assert_eq!(memory.read(DATA, 4), b"data");
    }

    /// Mapping garbage-filled pages must not leak into the program: the loader relies on
    /// `map_zeroed`, so a memory that doesn't zero shows 0xff right after the file bytes
    #[test]
    fn the_fake_memory_would_show_missing_zeroing() {
        let mut memory = FakeMemory { pages: BTreeMap::new(), zero: false };
        load(&hello().build()[..], &mut memory, USER_END).unwrap();
        assert_eq!(memory.read(DATA + 4, 1), [0xff]);
    }

    /// Random corruption of a valid file: no panic, and whatever loads stays in mapped pages
    #[test]
    fn corrupted_files_never_panic() {
        let good = hello().header(PT_TLS, segment(0, PF_R, 0, 8)).build();
        let mut seed = 0x0e1f_2026u64;
        let mut random = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let table_end = HEADER_SIZE + 3 * PHENT_SIZE as usize;
        for _ in 0..5000 {
            let mut file = good.clone();
            for _ in 0..1 + random() % 4 {
                let at = (random() % table_end as u64) as usize;
                file[at] = match random() % 3 {
                    0 => 0,
                    1 => 0xff,
                    _ => random() as u8,
                };
            }
            let mut memory = FakeMemory::new();
            if let Ok(program) = load(&file[..], &mut memory, USER_END) {
                for segment in &program.segments {
                    assert!(segment.vaddr >= MIN_USER_ADDR && segment.end() <= USER_END);
                }
                for (flags, _) in memory.pages.values() {
                    assert!(flags.contains(PageTableFlags::NO_EXECUTE) || !flags.contains(PageTableFlags::WRITABLE), "writable code");
                }
                assert!(memory.flags(program.entry).is_some_and(|flags| !flags.contains(PageTableFlags::NO_EXECUTE)));
            }
        }
    }
}
