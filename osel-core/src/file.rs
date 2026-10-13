//! Open files of a program: what a descriptor refers to and the descriptor table
//!
//! Like Linux, three layers: a `FileLike` is the thing itself (a file of the file system,
//! a device), an `OpenDescription` is one `open` of it with its offset and access mode,
//! shared by `dup`, and an `FdTable` maps descriptor numbers to descriptions.

use crate::errno::{Errno, KResult};
use crate::sync::Spinlock;
use crate::vfs::{FileType, Inode, Stat};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

/// Something a descriptor can refer to. Whatever it can't do fails with the error a Linux
/// program would get for that kind of file.
pub trait FileLike: Send + Sync {
    /// Reads at `offset` (ignored by files without one), returns the bytes read, 0 at the end.
    /// A file that waits for input returns `EAGAIN` instead when `nonblock` is set.
    fn read(&self, _offset: u64, _buf: &mut [u8], _nonblock: bool) -> KResult<usize> {
        Err(Errno::EINVAL)
    }

    /// Writes at `offset` (ignored by files without one), returns the bytes written
    fn write(&self, _offset: u64, _buf: &[u8]) -> KResult<usize> {
        Err(Errno::EINVAL)
    }

    fn stat(&self) -> Stat;

    /// The node of the file system behind it, for `openat` relative to a directory
    fn inode(&self) -> Option<Arc<dyn Inode>> {
        None
    }

    /// Whether reads and writes move an offset, like a regular file; devices don't have one
    fn seekable(&self) -> bool {
        false
    }

    /// Whether reading and writing would not block right now. A regular file is always
    /// ready; `epoll` asks this of the descriptors it watches.
    fn poll(&self) -> Ready {
        Ready { readable: true, writable: true }
    }

    /// Whether `epoll` may watch it. Only files that can become ready later (an `eventfd`,
    /// a pipe) say yes; `epoll_ctl` of anything else is `EPERM`, which Go relies on to
    /// treat regular files as always ready.
    fn epollable(&self) -> bool {
        false
    }

    /// The `epoll` instance behind this file, for `epoll_ctl` and `epoll_pwait`
    fn as_epoll(&self) -> Option<&crate::poll::Epoll> {
        None
    }
}

/// Whether a file can be read or written now, the answer to `poll`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ready {
    pub readable: bool,
    pub writable: bool,
}

/// A file or directory of a file system
pub struct InodeFile {
    inode: Arc<dyn Inode>,
}

impl InodeFile {
    pub fn new(inode: Arc<dyn Inode>) -> InodeFile {
        InodeFile { inode }
    }
}

impl FileLike for InodeFile {
    fn read(&self, offset: u64, buf: &mut [u8], _nonblock: bool) -> KResult<usize> {
        self.inode.read_at(offset, buf)
    }

    fn write(&self, _offset: u64, _buf: &[u8]) -> KResult<usize> {
        // only reachable through a writable descriptor, which a read-only file system never gives
        Err(Errno::EROFS)
    }

    fn stat(&self) -> Stat {
        self.inode.stat()
    }

    fn inode(&self) -> Option<Arc<dyn Inode>> {
        Some(self.inode.clone())
    }

    fn seekable(&self) -> bool {
        true
    }
}

/// `/dev/null`: reads nothing, swallows everything
pub struct NullFile {
    stat: Stat,
}

impl NullFile {
    /// `stat` is the device node's
    pub fn new(stat: Stat) -> NullFile {
        NullFile { stat }
    }
}

impl FileLike for NullFile {
    fn read(&self, _offset: u64, _buf: &mut [u8], _nonblock: bool) -> KResult<usize> {
        Ok(0)
    }

    fn write(&self, _offset: u64, buf: &[u8]) -> KResult<usize> {
        Ok(buf.len())
    }

    fn stat(&self) -> Stat {
        self.stat
    }
}

pub const O_RDONLY: u32 = 0;
pub const O_WRONLY: u32 = 1;
pub const O_RDWR: u32 = 2;
pub const O_ACCMODE: u32 = 3;
pub const O_CREAT: u32 = 0o100;
pub const O_EXCL: u32 = 0o200;
pub const O_TRUNC: u32 = 0o1000;
pub const O_APPEND: u32 = 0o2000;
pub const O_NONBLOCK: u32 = 0o4000;
pub const O_LARGEFILE: u32 = 0o100000;
pub const O_DIRECTORY: u32 = 0o200000;
pub const O_CLOEXEC: u32 = 0o2000000;
pub const O_PATH: u32 = 0o10000000;
/// `O_TMPFILE` without its `O_DIRECTORY` bit
pub const O_TMPFILE_BIT: u32 = 0o20000000;
/// Status flags kept by the description and reported by `fcntl(F_GETFL)`
pub const STATUS_FLAGS: u32 = O_APPEND | O_NONBLOCK;

/// What an open allows
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Access {
    pub read: bool,
    pub write: bool,
}

/// Checks `flags` of `openat` against what the path names, in the order Linux does, on a
/// file system that can't be written: `None` if the last name of the path doesn't exist.
/// Returns what the descriptor may do.
pub fn open_access(flags: u32, node: Option<FileType>) -> KResult<Access> {
    // creating, even an unnamed file, needs a writable file system
    if flags & O_TMPFILE_BIT != 0 {
        return Err(Errno::EROFS);
    }
    if flags & O_PATH != 0 {
        return Err(Errno::EINVAL);
    }
    let Some(file_type) = node else {
        return Err(if flags & O_CREAT != 0 { Errno::EROFS } else { Errno::ENOENT });
    };
    if flags & (O_CREAT | O_EXCL) == O_CREAT | O_EXCL {
        return Err(Errno::EEXIST);
    }
    let mode = flags & O_ACCMODE;
    let access = Access { read: mode == O_RDONLY || mode == O_RDWR, write: mode == O_WRONLY || mode == O_RDWR };
    // Linux asks for write permission on O_TRUNC, and for both on access mode 3
    let needs_write = access.write || flags & O_TRUNC != 0 || mode == O_ACCMODE;
    match file_type {
        FileType::Directory => {
            if flags & O_CREAT != 0 || needs_write {
                return Err(Errno::EISDIR);
            }
        }
        _ if flags & O_DIRECTORY != 0 => return Err(Errno::ENOTDIR),
        FileType::Regular => {
            if needs_write {
                return Err(Errno::EROFS);
            }
        }
        // devices open for writing, O_TRUNC means nothing to them
        _ => {}
    }
    Ok(access)
}

/// One `open` of a file: shared by `dup`, so the offset and the status flags are too
pub struct OpenDescription {
    file: Arc<dyn FileLike>,
    access: Access,
    status: AtomicU32,
    offset: Spinlock<u64>,
}

impl OpenDescription {
    pub fn new(file: Arc<dyn FileLike>, access: Access, flags: u32) -> OpenDescription {
        OpenDescription { file, access, status: AtomicU32::new(flags & STATUS_FLAGS), offset: Spinlock::new("OFFSET", 0) }
    }

    /// `read`: `EBADF` without read access. The offset moves only by what was read.
    pub fn read(&self, buf: &mut [u8]) -> KResult<usize> {
        if !self.access.read {
            return Err(Errno::EBADF);
        }
        let nonblock = self.status.load(Ordering::Relaxed) & O_NONBLOCK != 0;
        if !self.file.seekable() {
            // a device may wait for input: no lock is held meanwhile
            return self.file.read(0, buf, nonblock);
        }
        let mut offset = self.offset.lock();
        let read = self.file.read(*offset, buf, nonblock)?;
        *offset += read as u64;
        Ok(read)
    }

    /// `write`: `EBADF` without write access
    pub fn write(&self, buf: &[u8]) -> KResult<usize> {
        if !self.access.write {
            return Err(Errno::EBADF);
        }
        if !self.file.seekable() {
            return self.file.write(0, buf);
        }
        let mut offset = self.offset.lock();
        let written = self.file.write(*offset, buf)?;
        *offset += written as u64;
        Ok(written)
    }

    pub fn stat(&self) -> Stat {
        self.file.stat()
    }

    pub fn file(&self) -> &Arc<dyn FileLike> {
        &self.file
    }

    /// `fcntl(F_GETFL)`: access mode and status flags, with `O_LARGEFILE` as x86_64 Linux
    /// always reports it
    pub fn flags(&self) -> u32 {
        let mode = match (self.access.read, self.access.write) {
            (true, false) => O_RDONLY,
            (false, true) => O_WRONLY,
            (true, true) => O_RDWR,
            (false, false) => O_ACCMODE,
        };
        mode | O_LARGEFILE | self.status.load(Ordering::Relaxed)
    }

    /// `fcntl(F_SETFL)`: only the status flags change
    pub fn set_status(&self, flags: u32) {
        self.status.store(flags & STATUS_FLAGS, Ordering::Relaxed);
    }

    pub fn offset(&self) -> u64 {
        *self.offset.lock()
    }
}

/// Descriptors a program may have open at once, `RLIMIT_NOFILE` of Linux by default
pub const MAX_FDS: usize = 1024;

#[derive(Clone)]
struct FdEntry {
    description: Arc<OpenDescription>,
    /// Closed on `execve`: per descriptor, not shared by `dup`
    cloexec: bool,
}

/// The descriptors of a program
#[derive(Default, Clone)]
pub struct FdTable {
    slots: Vec<Option<FdEntry>>,
}

impl FdTable {
    pub fn new() -> FdTable {
        FdTable { slots: Vec::new() }
    }

    /// Puts `description` at the lowest free number, `EMFILE` if all are taken
    pub fn insert(&mut self, description: Arc<OpenDescription>, cloexec: bool) -> KResult<i32> {
        let entry = Some(FdEntry { description, cloexec });
        let fd = match self.slots.iter().position(Option::is_none) {
            Some(free) => free,
            None if self.slots.len() < MAX_FDS => {
                self.slots.push(None);
                self.slots.len() - 1
            }
            None => return Err(Errno::EMFILE),
        };
        self.slots[fd] = entry;
        Ok(fd as i32)
    }

    fn entry(&self, fd: i32) -> KResult<&FdEntry> {
        let index = usize::try_from(fd).map_err(|_| Errno::EBADF)?;
        self.slots.get(index).and_then(Option::as_ref).ok_or(Errno::EBADF)
    }

    /// The description behind `fd`, to use without holding the table
    pub fn get(&self, fd: i32) -> KResult<Arc<OpenDescription>> {
        Ok(self.entry(fd)?.description.clone())
    }

    pub fn close(&mut self, fd: i32) -> KResult<()> {
        self.entry(fd)?;
        self.slots[fd as usize] = None;
        while self.slots.last().is_some_and(Option::is_none) {
            self.slots.pop();
        }
        Ok(())
    }

    /// A new descriptor for the same description, at the lowest free number not below
    /// `min`, without close-on-exec unless `cloexec`
    pub fn dup(&mut self, fd: i32, min: i32, cloexec: bool) -> KResult<i32> {
        let description = self.get(fd)?;
        let min = usize::try_from(min).ok().filter(|&min| min < MAX_FDS).ok_or(Errno::EINVAL)?;
        let free = (min..MAX_FDS).find(|&index| self.slots.get(index).is_none_or(Option::is_none)).ok_or(Errno::EMFILE)?;
        if free >= self.slots.len() {
            self.slots.resize(free + 1, None);
        }
        self.slots[free] = Some(FdEntry { description, cloexec });
        Ok(free as i32)
    }

    pub fn cloexec(&self, fd: i32) -> KResult<bool> {
        Ok(self.entry(fd)?.cloexec)
    }

    pub fn set_cloexec(&mut self, fd: i32, cloexec: bool) -> KResult<()> {
        self.entry(fd)?;
        if let Some(entry) = self.slots[fd as usize].as_mut() {
            entry.cloexec = cloexec;
        }
        Ok(())
    }

    /// Number of open descriptors
    pub fn len(&self) -> usize {
        self.slots.iter().flatten().count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Size of `struct stat` on x86_64 Linux
pub const STAT_SIZE: usize = 144;
/// `st_dev` of every file: OSel has one file system
pub const ROOT_DEVICE: u64 = 1;

/// `struct stat` of x86_64 Linux for `stat`: times are 0, the block size 4096
pub fn encode_stat(stat: &Stat) -> [u8; STAT_SIZE] {
    let mut bytes = [0u8; STAT_SIZE];
    let mut put = |at: usize, value: &[u8]| bytes[at..at + value.len()].copy_from_slice(value);
    let nlink = match (stat.nlink, stat.file_type) {
        (0, FileType::Directory) => 2,
        (0, _) => 1,
        (nlink, _) => nlink,
    };
    put(0, &ROOT_DEVICE.to_le_bytes());
    put(8, &stat.ino.to_le_bytes());
    put(16, &nlink.to_le_bytes());
    put(24, &stat.mode().to_le_bytes());
    // uid and gid are 0: everything belongs to root
    put(40, &stat.rdev.to_le_bytes());
    put(48, &stat.size.to_le_bytes());
    put(56, &4096u64.to_le_bytes());
    put(64, &stat.size.div_ceil(512).to_le_bytes());
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::MemBlockDevice;
    use crate::cpio::{CpioBuilder, CpioFs};
    use crate::devices::{NULL, WithDevices};
    use crate::vfs::{FileSystem, resolve};
    use alloc::collections::BTreeMap;
    use alloc::vec;

    fn file(data: &[u8]) -> Arc<dyn FileLike> {
        let mut builder = CpioBuilder::new();
        builder.add_file("f", 0o644, data);
        let fs = CpioFs::mount(Arc::new(MemBlockDevice::new(builder.finish()))).unwrap();
        let root = fs.root();
        Arc::new(InodeFile::new(resolve(&root, &root, "/f").unwrap()))
    }

    const READ: Access = Access { read: true, write: false };

    #[test]
    fn open_follows_linux_order() {
        use FileType::*;
        let cases: [(u32, Option<FileType>, KResult<Access>); 21] = [
            (O_RDONLY, Some(Regular), Ok(READ)),
            (O_RDONLY, None, Err(Errno::ENOENT)),
            (O_CREAT, None, Err(Errno::EROFS)),
            (O_CREAT | O_EXCL, Some(Regular), Err(Errno::EEXIST)),
            (O_CREAT, Some(Regular), Ok(READ)),
            (O_CREAT, Some(Directory), Err(Errno::EISDIR)),
            (O_WRONLY, Some(Directory), Err(Errno::EISDIR)),
            (O_WRONLY, Some(Regular), Err(Errno::EROFS)),
            (O_RDWR, Some(Regular), Err(Errno::EROFS)),
            (O_RDONLY | O_TRUNC, Some(Regular), Err(Errno::EROFS)),
            (O_RDONLY | O_DIRECTORY, Some(Regular), Err(Errno::ENOTDIR)),
            (O_RDONLY | O_DIRECTORY, Some(CharDevice), Err(Errno::ENOTDIR)),
            (O_RDONLY | O_DIRECTORY, Some(Directory), Ok(READ)),
            (O_RDWR, Some(CharDevice), Ok(Access { read: true, write: true })),
            (O_WRONLY | O_TRUNC, Some(CharDevice), Ok(Access { read: false, write: true })),
            (O_TMPFILE_BIT | O_DIRECTORY | O_RDWR, Some(Directory), Err(Errno::EROFS)),
            (O_PATH, Some(Regular), Err(Errno::EINVAL)),
            (O_ACCMODE, Some(CharDevice), Ok(Access { read: false, write: false })),
            (O_ACCMODE, Some(Regular), Err(Errno::EROFS)),
            (O_RDONLY | O_TRUNC, Some(Directory), Err(Errno::EISDIR)),
            (O_ACCMODE, Some(Directory), Err(Errno::EISDIR)),
        ];
        for (flags, node, expected) in cases {
            assert_eq!(open_access(flags, node), expected, "flags {flags:#o} on {node:?}");
        }
        assert_eq!(open_access(O_RDONLY | O_CLOEXEC | O_NONBLOCK | 0o10000000000, Some(Regular)), Ok(READ), "other bits are ignored");
    }

    #[test]
    fn reads_move_the_shared_offset() {
        let description = Arc::new(OpenDescription::new(file(b"hello world"), READ, 0));
        let mut table = FdTable::new();
        let fd = table.insert(description, false).unwrap();
        let copy = table.dup(fd, 0, false).unwrap();
        let mut buf = [0u8; 5];
        assert_eq!(table.get(fd).unwrap().read(&mut buf), Ok(5));
        assert_eq!(&buf, b"hello");
        assert_eq!(table.get(copy).unwrap().read(&mut buf), Ok(5), "dup shares the offset");
        assert_eq!(&buf, b" worl");
        let mut rest = [0u8; 16];
        assert_eq!(table.get(fd).unwrap().read(&mut rest), Ok(1));
        assert_eq!(table.get(fd).unwrap().read(&mut rest), Ok(0), "end of file");
        assert_eq!(table.get(fd).unwrap().write(b"x"), Err(Errno::EBADF), "read only");
    }

    #[test]
    fn descriptors_are_the_lowest_free_numbers() {
        let description = Arc::new(OpenDescription::new(file(b""), READ, 0));
        let mut table = FdTable::new();
        for expected in 0..5 {
            assert_eq!(table.insert(description.clone(), false), Ok(expected));
        }
        table.close(1).unwrap();
        table.close(3).unwrap();
        assert_eq!(table.insert(description.clone(), false), Ok(1));
        assert_eq!(table.dup(0, 0, true), Ok(3));
        assert_eq!(table.cloexec(3), Ok(true));
        assert_eq!(table.dup(0, 10, false), Ok(10), "F_DUPFD with a minimum");
        assert_eq!(table.insert(description.clone(), false), Ok(5));
        assert_eq!(table.close(3), Ok(()));
        assert_eq!(table.close(3), Err(Errno::EBADF), "closed twice");
        for bad in [-1, -100, 99, i32::MAX] {
            assert_eq!(table.get(bad).err(), Some(Errno::EBADF), "{bad}");
            assert_eq!(table.close(bad), Err(Errno::EBADF));
        }
        assert_eq!(table.dup(0, -1, false), Err(Errno::EINVAL));
    }

    #[test]
    fn the_table_is_limited() {
        let description = Arc::new(OpenDescription::new(file(b""), READ, 0));
        let mut table = FdTable::new();
        for _ in 0..MAX_FDS {
            table.insert(description.clone(), false).unwrap();
        }
        assert_eq!(table.insert(description.clone(), false), Err(Errno::EMFILE));
        assert_eq!(table.dup(0, 0, false), Err(Errno::EMFILE));
        assert_eq!(table.len(), MAX_FDS);
    }

    #[test]
    fn cloexec_is_per_descriptor_and_status_per_description() {
        let description = Arc::new(OpenDescription::new(file(b""), READ, O_NONBLOCK | O_CLOEXEC));
        let mut table = FdTable::new();
        let fd = table.insert(description, true).unwrap();
        let copy = table.dup(fd, 0, false).unwrap();
        assert_eq!((table.cloexec(fd), table.cloexec(copy)), (Ok(true), Ok(false)));
        assert_eq!(table.get(copy).unwrap().flags(), O_RDONLY | O_LARGEFILE | O_NONBLOCK);
        table.get(fd).unwrap().set_status(O_APPEND | O_WRONLY);
        assert_eq!(table.get(copy).unwrap().flags(), O_RDONLY | O_LARGEFILE | O_APPEND, "status is shared, the access mode can't change");
        table.set_cloexec(copy, true).unwrap();
        assert_eq!(table.cloexec(copy), Ok(true));
    }

    #[test]
    fn random_operations_match_a_model() {
        let description = Arc::new(OpenDescription::new(file(b""), READ, 0));
        let mut table = FdTable::new();
        let mut model: BTreeMap<i32, bool> = BTreeMap::new();
        let mut seed = 0xf0d5u64;
        let mut random = move |limit: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % limit
        };
        for _ in 0..5000 {
            let lowest = (0..).find(|fd| !model.contains_key(fd)).unwrap();
            match random(3) {
                0 if lowest as usize == MAX_FDS => assert_eq!(table.insert(description.clone(), false), Err(Errno::EMFILE)),
                0 => {
                    assert_eq!(table.insert(description.clone(), false), Ok(lowest));
                    model.insert(lowest, false);
                }
                1 => {
                    let fd = random(64) as i32;
                    assert_eq!(table.close(fd).is_ok(), model.remove(&fd).is_some(), "close {fd}");
                }
                _ => {
                    let fd = random(64) as i32;
                    let result = table.dup(fd, 0, true);
                    if model.contains_key(&fd) && lowest as usize == MAX_FDS {
                        assert_eq!(result, Err(Errno::EMFILE));
                    } else if model.contains_key(&fd) {
                        assert_eq!(result, Ok(lowest));
                        model.insert(lowest, true);
                    } else {
                        assert_eq!(result, Err(Errno::EBADF));
                    }
                }
            }
            assert_eq!(table.len(), model.len());
        }
        for (fd, cloexec) in model {
            assert_eq!(table.cloexec(fd), Ok(cloexec));
        }
    }

    #[test]
    fn null_reads_nothing_and_takes_everything() {
        let fs = WithDevices::new(Arc::new(CpioFs::mount(Arc::new(MemBlockDevice::new(CpioBuilder::new().finish()))).unwrap()));
        let root = fs.root();
        let node = resolve(&root, &root, "/dev/null").unwrap();
        let null = OpenDescription::new(Arc::new(NullFile::new(node.stat())), Access { read: true, write: true }, 0);
        assert_eq!(null.read(&mut [0u8; 8]), Ok(0));
        assert_eq!(null.write(&vec![7u8; 10_000]), Ok(10_000));
        assert_eq!(null.stat().rdev, NULL);
        assert_eq!(null.offset(), 0);
    }

    /// The layout of x86_64 `struct stat`, field by field
    #[test]
    fn stat_is_the_linux_layout() {
        let stat = Stat { ino: 7, file_type: FileType::Regular, permissions: 0o644, size: 1000, ..Stat::default() };
        let bytes = encode_stat(&stat);
        let word = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
        let half = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        assert_eq!(word(0), ROOT_DEVICE, "st_dev");
        assert_eq!(word(8), 7, "st_ino");
        assert_eq!(word(16), 1, "st_nlink");
        assert_eq!(half(24), 0o100644, "st_mode");
        assert_eq!((half(28), half(32)), (0, 0), "st_uid, st_gid");
        assert_eq!(word(40), 0, "st_rdev");
        assert_eq!(word(48), 1000, "st_size");
        assert_eq!(word(56), 4096, "st_blksize");
        assert_eq!(word(64), 2, "st_blocks of 512 bytes");
        assert!(bytes[72..].iter().all(|&b| b == 0), "times and the reserved words");
        let dir = encode_stat(&Stat { file_type: FileType::Directory, ..Stat::default() });
        assert_eq!(u64::from_le_bytes(dir[16..24].try_into().unwrap()), 2, "a directory has 2 links");
        let device = encode_stat(&Stat { file_type: FileType::CharDevice, rdev: NULL, permissions: 0o666, ..Stat::default() });
        assert_eq!(u32::from_le_bytes(device[24..28].try_into().unwrap()), 0o020666);
        assert_eq!(u64::from_le_bytes(device[40..48].try_into().unwrap()), NULL);
    }
}
