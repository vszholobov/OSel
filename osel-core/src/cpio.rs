//! A read-only file system over a cpio archive in the "newc" format, the format
//! of Linux initramfs, and a writer for such archives
//!
//! `CpioFs::mount` walks the headers once and keeps the tree in memory; file
//! contents stay on the block device and are read on demand. The runner packs
//! the ramdisk with `CpioBuilder`, tests use it to build archives on the fly.
//!
//! Supported entries: regular files and directories. Symlinks, device nodes and
//! anything else fail the mount with `EINVAL`, as do malformed headers, paths
//! with `..`, the same path twice and a missing trailer.

use crate::block::{BlockDevice, read_bytes};
use crate::errno::{Errno, KResult};
use crate::vfs::{DirEntry, FileSystem, FileType, Inode, NAME_MAX, PATH_MAX, Stat};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

const MAGIC: &[u8; 6] = b"070701";
/// The same layout with a checksum of the data, which is not verified
const MAGIC_CRC: &[u8; 6] = b"070702";
const HEADER_SIZE: usize = 110;
const TRAILER: &str = "TRAILER!!!";
const S_IFMT: u32 = 0o170000;
const S_IFREG: u32 = 0o100000;
const S_IFDIR: u32 = 0o040000;
/// Permission bits of directories the archive implies but does not list
const IMPLICIT_DIR_PERMISSIONS: u32 = 0o755;

/// The fields of a newc header, all hex encoded in the archive
struct Header {
    mode: u32,
    file_size: u32,
    name_size: u32,
}

impl Header {
    fn parse(raw: &[u8; HEADER_SIZE]) -> KResult<Self> {
        if &raw[..6] != MAGIC && &raw[..6] != MAGIC_CRC {
            return Err(Errno::EINVAL);
        }
        // 13 fields of 8 hex digits: ino, mode, uid, gid, nlink, mtime, filesize,
        // devmajor, devminor, rdevmajor, rdevminor, namesize, check
        let field = |index: usize| -> KResult<u32> {
            let digits = &raw[6 + index * 8..6 + (index + 1) * 8];
            if !digits.iter().all(u8::is_ascii_hexdigit) {
                return Err(Errno::EINVAL);
            }
            let text = core::str::from_utf8(digits).map_err(|_| Errno::EINVAL)?;
            u32::from_str_radix(text, 16).map_err(|_| Errno::EINVAL)
        };
        Ok(Header { mode: field(1)?, file_size: field(6)?, name_size: field(11)? })
    }
}

const fn align4(value: u64) -> u64 {
    (value + 3) & !3
}

enum Kind {
    File { offset: u64, size: u64 },
    Dir { children: BTreeMap<String, usize>, listed: bool },
}

struct Node {
    permissions: u32,
    kind: Kind,
}

struct Tree {
    device: Arc<dyn BlockDevice>,
    /// The root is node 0; the inode number of node `i` is `i + 1`
    nodes: Vec<Node>,
}

impl Tree {
    /// Adds the entry `path`, creating missing parent directories
    fn insert(&mut self, path: &str, permissions: u32, kind: Kind) -> KResult<()> {
        let names: Vec<&str> = path.split('/').filter(|name| !matches!(*name, "" | ".")).collect();
        let Some((last, parents)) = names.split_last() else {
            // the archive lists the root itself, `.`, `./` or `/`
            return match (kind, &mut self.nodes[0].kind) {
                (Kind::Dir { .. }, Kind::Dir { listed, .. }) if !*listed => {
                    *listed = true;
                    self.nodes[0].permissions = permissions;
                    Ok(())
                }
                _ => Err(Errno::EINVAL),
            };
        };
        if names.iter().any(|name| *name == ".." || name.len() > NAME_MAX) {
            return Err(Errno::EINVAL);
        }
        let mut parent = 0;
        for name in parents {
            parent = match self.child(parent, name)? {
                Some(index) => index,
                None => self.add_child(
                    parent,
                    name,
                    Node {
                        permissions: IMPLICIT_DIR_PERMISSIONS,
                        kind: Kind::Dir { children: BTreeMap::new(), listed: false },
                    },
                ),
            };
        }
        match self.child(parent, last)? {
            None => {
                self.add_child(parent, last, Node { permissions, kind });
                Ok(())
            }
            Some(existing) => match (&mut self.nodes[existing].kind, kind) {
                // a directory listed after its own contents
                (Kind::Dir { listed, .. }, Kind::Dir { .. }) if !*listed => {
                    *listed = true;
                    self.nodes[existing].permissions = permissions;
                    Ok(())
                }
                _ => Err(Errno::EINVAL),
            },
        }
    }

    /// The child `name` of directory `parent`; `EINVAL` if `parent` is a file
    fn child(&self, parent: usize, name: &str) -> KResult<Option<usize>> {
        match &self.nodes[parent].kind {
            Kind::Dir { children, .. } => Ok(children.get(name).copied()),
            Kind::File { .. } => Err(Errno::EINVAL),
        }
    }

    fn add_child(&mut self, parent: usize, name: &str, node: Node) -> usize {
        let index = self.nodes.len();
        self.nodes.push(node);
        let Kind::Dir { children, .. } = &mut self.nodes[parent].kind else {
            unreachable!("`child` checked that the parent is a directory")
        };
        children.insert(String::from(name), index);
        index
    }
}

/// A mounted cpio archive
pub struct CpioFs {
    tree: Arc<Tree>,
}

impl CpioFs {
    /// Reads the whole directory tree of the archive on `device`
    pub fn mount(device: Arc<dyn BlockDevice>) -> KResult<Self> {
        let capacity = device.num_blocks().checked_mul(device.block_size() as u64).ok_or(Errno::EINVAL)?;
        let root = Node { permissions: IMPLICIT_DIR_PERMISSIONS, kind: Kind::Dir { children: BTreeMap::new(), listed: false } };
        let mut tree = Tree { device, nodes: vec![root] };
        let mut offset = 0u64;
        loop {
            // a header or a name past the end means the archive was cut off
            let truncated = |e: Errno| if e == Errno::EIO { Errno::EINVAL } else { e };
            let mut raw = [0u8; HEADER_SIZE];
            read_bytes(&*tree.device, offset, &mut raw).map_err(truncated)?;
            let header = Header::parse(&raw)?;
            let name_size = header.name_size as usize;
            if name_size == 0 || name_size > PATH_MAX {
                return Err(Errno::EINVAL);
            }
            let mut name = vec![0u8; name_size];
            read_bytes(&*tree.device, offset + HEADER_SIZE as u64, &mut name).map_err(truncated)?;
            // the name ends with exactly one zero byte
            if name.pop() != Some(0) || name.contains(&0) {
                return Err(Errno::EINVAL);
            }
            let name = String::from_utf8(name).map_err(|_| Errno::EINVAL)?;
            if name == TRAILER {
                break;
            }
            let data_offset = align4(offset + HEADER_SIZE as u64 + name_size as u64);
            let size = u64::from(header.file_size);
            let data_end = data_offset + size;
            if data_end > capacity {
                return Err(Errno::EINVAL);
            }
            let permissions = header.mode & 0o7777;
            let kind = match header.mode & S_IFMT {
                S_IFREG => Kind::File { offset: data_offset, size },
                S_IFDIR if size == 0 => Kind::Dir { children: BTreeMap::new(), listed: true },
                _ => return Err(Errno::EINVAL),
            };
            tree.insert(&name, permissions, kind)?;
            offset = align4(data_end);
        }
        Ok(CpioFs { tree: Arc::new(tree) })
    }

    /// Number of files and directories, the root included
    pub fn node_count(&self) -> usize {
        self.tree.nodes.len()
    }
}

impl FileSystem for CpioFs {
    fn root(&self) -> Arc<dyn Inode> {
        Arc::new(CpioInode { tree: self.tree.clone(), index: 0 })
    }

    fn name(&self) -> &'static str {
        "cpio"
    }
}

struct CpioInode {
    tree: Arc<Tree>,
    index: usize,
}

impl CpioInode {
    fn node(&self) -> &Node {
        &self.tree.nodes[self.index]
    }

    fn stat_of(&self, index: usize) -> Stat {
        let node = &self.tree.nodes[index];
        let (file_type, size) = match node.kind {
            Kind::File { size, .. } => (FileType::Regular, size),
            Kind::Dir { .. } => (FileType::Directory, 0),
        };
        Stat { ino: index as u64 + 1, file_type, permissions: node.permissions, size }
    }

    fn children(&self) -> KResult<&BTreeMap<String, usize>> {
        match &self.node().kind {
            Kind::Dir { children, .. } => Ok(children),
            Kind::File { .. } => Err(Errno::ENOTDIR),
        }
    }
}

impl Inode for CpioInode {
    fn stat(&self) -> Stat {
        self.stat_of(self.index)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> KResult<usize> {
        let Kind::File { offset: start, size } = self.node().kind else {
            return Err(Errno::EISDIR);
        };
        if offset >= size {
            return Ok(0);
        }
        let len = (size - offset).min(buf.len() as u64) as usize;
        read_bytes(&*self.tree.device, start + offset, &mut buf[..len])?;
        Ok(len)
    }

    fn lookup(&self, name: &str) -> KResult<Arc<dyn Inode>> {
        let index = *self.children()?.get(name).ok_or(Errno::ENOENT)?;
        Ok(Arc::new(CpioInode { tree: self.tree.clone(), index }))
    }

    fn read_dir(&self, index: usize) -> KResult<Option<DirEntry>> {
        Ok(self.children()?.iter().nth(index).map(|(name, &child)| {
            let stat = self.stat_of(child);
            DirEntry { name: name.clone(), ino: stat.ino, file_type: stat.file_type }
        }))
    }
}

/// Writes a newc archive. Entries get consecutive inode numbers and zero owner and
/// time, so the same input gives the same bytes.
pub struct CpioBuilder {
    data: Vec<u8>,
    next_ino: u32,
}

impl Default for CpioBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl CpioBuilder {
    pub fn new() -> Self {
        Self { data: Vec::new(), next_ino: 1 }
    }

    /// Adds a directory; `path` is relative to the root, without a leading `/`
    pub fn add_dir(&mut self, path: &str, permissions: u32) -> &mut Self {
        self.entry(path, S_IFDIR | (permissions & 0o7777), 2, &[]);
        self
    }

    /// Adds a regular file; panics if `data` is 4 GiB or more, which newc can't store
    pub fn add_file(&mut self, path: &str, permissions: u32, data: &[u8]) -> &mut Self {
        self.entry(path, S_IFREG | (permissions & 0o7777), 1, data);
        self
    }

    /// Appends the trailer and pads the archive to whole 512 byte blocks, as cpio does
    pub fn finish(mut self) -> Vec<u8> {
        self.next_ino = 0;
        self.entry(TRAILER, 0, 1, &[]);
        let padded = self.data.len().next_multiple_of(512);
        self.data.resize(padded, 0);
        self.data
    }

    fn entry(&mut self, name: &str, mode: u32, nlink: u32, data: &[u8]) {
        assert!(!name.contains('\0'), "cpio entry name contains a zero byte: {name:?}");
        let file_size = u32::try_from(data.len()).expect("a cpio newc entry must be smaller than 4 GiB");
        let name_size = u32::try_from(name.len() + 1).expect("cpio entry name is too long");
        let ino = self.next_ino;
        self.next_ino += 1;
        let fields = [ino, mode, 0, 0, nlink, 0, file_size, 0, 0, 0, 0, name_size, 0];
        self.data.extend_from_slice(MAGIC);
        for field in fields {
            self.data.extend_from_slice(alloc::format!("{field:08X}").as_bytes());
        }
        self.data.extend_from_slice(name.as_bytes());
        self.data.push(0);
        self.pad();
        self.data.extend_from_slice(data);
        self.pad();
    }

    fn pad(&mut self) {
        let aligned = align4(self.data.len() as u64) as usize;
        self.data.resize(aligned, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::MemBlockDevice;
    use crate::vfs::{list_dir, read_to_end, resolve};

    fn mount(archive: Vec<u8>) -> KResult<CpioFs> {
        CpioFs::mount(Arc::new(MemBlockDevice::new(archive.leak())))
    }

    fn read(fs: &CpioFs, path: &str) -> KResult<Vec<u8>> {
        read_to_end(&*resolve(&fs.root(), path)?)
    }

    fn names(fs: &CpioFs, path: &str) -> Vec<String> {
        list_dir(&*resolve(&fs.root(), path).unwrap()).unwrap().into_iter().map(|entry| entry.name).collect()
    }

    fn sample() -> Vec<u8> {
        let mut builder = CpioBuilder::new();
        builder
            .add_dir("etc", 0o755)
            .add_file("etc/motd", 0o644, b"Hello from the ramdisk\n")
            .add_dir("bin", 0o700)
            .add_file("bin/empty", 0o755, b"")
            .add_file("big", 0o600, &pattern(5000));
        builder.finish()
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 7 % 256) as u8).collect()
    }

    #[test]
    fn mounts_what_the_builder_wrote() {
        let fs = mount(sample()).unwrap();
        assert_eq!(fs.name(), "cpio");
        assert_eq!(fs.node_count(), 6);
        assert_eq!(read(&fs, "/etc/motd"), Ok(b"Hello from the ramdisk\n".to_vec()));
        assert_eq!(read(&fs, "/bin/empty"), Ok(Vec::new()));
        assert_eq!(read(&fs, "/big"), Ok(pattern(5000)));
        assert_eq!(names(&fs, "/"), ["big", "bin", "etc"]);
        assert_eq!(names(&fs, "/etc"), ["motd"]);
    }

    #[test]
    fn stat_reports_type_permissions_and_size() {
        let fs = mount(sample()).unwrap();
        let stat = |path| resolve(&fs.root(), path).unwrap().stat();
        assert_eq!(stat("/").ino, 1);
        assert_eq!(stat("/").file_type, FileType::Directory);
        assert_eq!(stat("/bin").permissions, 0o700);
        assert_eq!(stat("/big").mode(), 0o100600);
        assert_eq!(stat("/big").size, 5000);
        let mut inos: Vec<u64> = ["/", "/etc", "/etc/motd", "/bin", "/bin/empty", "/big"].map(|p| stat(p).ino).to_vec();
        inos.sort();
        inos.dedup();
        assert_eq!(inos.len(), 6, "inode numbers must be unique");
    }

    #[test]
    fn dir_entries_carry_inode_and_type() {
        let fs = mount(sample()).unwrap();
        let root = fs.root();
        let entries = list_dir(&*root).unwrap();
        for entry in entries {
            let child = root.lookup(&entry.name).unwrap().stat();
            assert_eq!((entry.ino, entry.file_type), (child.ino, child.file_type), "{}", entry.name);
        }
    }

    #[test]
    fn read_at_handles_offsets_and_the_end() {
        let fs = mount(sample()).unwrap();
        let big = resolve(&fs.root(), "/big").unwrap();
        let data = pattern(5000);
        let mut buf = [0u8; 600];
        for offset in [0, 1, 511, 512, 4400, 4999] {
            let read = big.read_at(offset, &mut buf).unwrap();
            let expected = (5000 - offset as usize).min(600);
            assert_eq!(read, expected, "offset {offset}");
            assert_eq!(&buf[..read], &data[offset as usize..offset as usize + read]);
        }
        assert_eq!(big.read_at(5000, &mut buf), Ok(0));
        assert_eq!(big.read_at(u64::MAX, &mut buf), Ok(0));
        assert_eq!(fs.root().read_at(0, &mut buf), Err(Errno::EISDIR));
    }

    #[test]
    fn lookup_errors() {
        let fs = mount(sample()).unwrap();
        assert_eq!(read(&fs, "/nope").err(), Some(Errno::ENOENT));
        assert_eq!(read(&fs, "/etc/motd/x").err(), Some(Errno::ENOTDIR));
        assert_eq!(read(&fs, "/etc").err(), Some(Errno::EISDIR));
    }

    #[test]
    fn empty_archive_has_an_empty_root() {
        let fs = mount(CpioBuilder::new().finish()).unwrap();
        assert_eq!(fs.node_count(), 1);
        assert!(names(&fs, "/").is_empty());
    }

    #[test]
    fn archive_is_padded_to_blocks() {
        assert_eq!(sample().len() % 512, 0);
        assert_eq!(CpioBuilder::new().finish().len(), 512);
    }

    #[test]
    fn builder_is_deterministic() {
        assert_eq!(sample(), sample());
    }

    #[test]
    fn parents_are_created_implicitly() {
        let mut builder = CpioBuilder::new();
        builder.add_file("a/b/c", 0o644, b"deep");
        let fs = mount(builder.finish()).unwrap();
        assert_eq!(read(&fs, "/a/b/c"), Ok(b"deep".to_vec()));
        assert_eq!(resolve(&fs.root(), "/a/b").unwrap().stat().permissions, IMPLICIT_DIR_PERMISSIONS);
    }

    #[test]
    fn directory_listed_after_its_contents_keeps_its_permissions() {
        let mut builder = CpioBuilder::new();
        builder.add_file("dir/file", 0o644, b"x").add_dir("dir", 0o711);
        let fs = mount(builder.finish()).unwrap();
        assert_eq!(resolve(&fs.root(), "/dir").unwrap().stat().permissions, 0o711);
        assert_eq!(names(&fs, "/dir"), ["file"]);
    }

    #[test]
    fn leading_dot_slash_and_root_entries() {
        // `find . | cpio -o -H newc` writes `.` first and `./`-less names after it
        let mut builder = CpioBuilder::new();
        builder.add_dir(".", 0o700).add_file("./x", 0o644, b"1").add_file("/y", 0o644, b"2");
        let fs = mount(builder.finish()).unwrap();
        assert_eq!(fs.root().stat().permissions, 0o700);
        assert_eq!(names(&fs, "/"), ["x", "y"]);
    }

    fn rejects(build: impl FnOnce(&mut CpioBuilder)) {
        let mut builder = CpioBuilder::new();
        build(&mut builder);
        assert_eq!(mount(builder.finish()).err(), Some(Errno::EINVAL));
    }

    #[test]
    fn rejects_duplicates_and_conflicts() {
        rejects(|b| {
            b.add_file("x", 0o644, b"1").add_file("x", 0o644, b"2");
        });
        rejects(|b| {
            b.add_dir("d", 0o755).add_dir("d", 0o755);
        });
        rejects(|b| {
            b.add_file("x", 0o644, b"1").add_file("x/y", 0o644, b"2");
        });
        rejects(|b| {
            b.add_file("x/y", 0o644, b"1").add_file("x", 0o644, b"2");
        });
        rejects(|b| {
            b.add_dir(".", 0o755).add_dir("/", 0o755);
        });
        rejects(|b| {
            b.add_file(".", 0o644, b"root as a file");
        });
    }

    #[test]
    fn rejects_dot_dot_and_long_names() {
        rejects(|b| {
            b.add_file("../escape", 0o644, b"");
        });
        rejects(|b| {
            b.add_file("a/../b", 0o644, b"");
        });
        rejects(|b| {
            b.add_file(&"n".repeat(NAME_MAX + 1), 0o644, b"");
        });
        let mut builder = CpioBuilder::new();
        builder.add_file(&"n".repeat(NAME_MAX), 0o644, b"ok");
        assert!(mount(builder.finish()).is_ok());
    }

    #[test]
    fn rejects_symlinks_and_devices() {
        for kind in [0o120000, 0o020000, 0o060000, 0o010000, 0o140000, 0] {
            let mut builder = CpioBuilder::new();
            builder.entry("odd", kind | 0o644, 1, b"target");
            assert_eq!(mount(builder.finish()).err(), Some(Errno::EINVAL), "mode {kind:o}");
        }
    }

    #[test]
    fn rejects_directory_with_data() {
        rejects(|b| b.entry("d", S_IFDIR | 0o755, 2, b"data"));
    }

    #[test]
    fn rejects_bad_magic_and_hex() {
        let mut archive = sample();
        archive[0] = b'1';
        assert_eq!(mount(archive).err(), Some(Errno::EINVAL));
        let mut archive = sample();
        archive[6 + 8] = b'g';
        assert_eq!(mount(archive).err(), Some(Errno::EINVAL));
        // a sign is accepted by from_str_radix, not by cpio
        let mut archive = sample();
        archive[6 + 6 * 8] = b'+';
        assert_eq!(mount(archive).err(), Some(Errno::EINVAL));
    }

    #[test]
    fn accepts_the_crc_variant() {
        let mut archive = sample();
        archive[5] = b'2';
        assert!(mount(archive).is_ok());
    }

    #[test]
    fn rejects_missing_trailer() {
        let mut builder = CpioBuilder::new();
        builder.add_file("x", 0o644, b"1");
        let data = builder.data.clone();
        assert_eq!(mount(data).err(), Some(Errno::EINVAL));
        // zeros after the last entry are not a trailer either
        let mut data = builder.data;
        data.resize(4096, 0);
        assert_eq!(mount(data).err(), Some(Errno::EINVAL));
        assert_eq!(mount(Vec::new()).err(), Some(Errno::EINVAL));
    }

    #[test]
    fn rejects_bad_names() {
        let with_name = |name: &[u8], name_size: u32| {
            let mut data = Vec::new();
            data.extend_from_slice(MAGIC);
            for field in [1, S_IFREG | 0o644, 0, 0, 1, 0, 0, 0, 0, 0, 0, name_size, 0] {
                data.extend_from_slice(std::format!("{field:08X}").as_bytes());
            }
            data.extend_from_slice(name);
            data.resize(align4(data.len() as u64) as usize, 0);
            let mut trailer = CpioBuilder::new().finish();
            data.append(&mut trailer);
            mount(data).err()
        };
        assert_eq!(with_name(b"ok\0", 3), None);
        assert_eq!(with_name(b"\0", 0), Some(Errno::EINVAL), "zero name size");
        assert_eq!(with_name(b"no-zero", 7), Some(Errno::EINVAL), "name without the zero byte");
        assert_eq!(with_name(b"a\0b\0", 4), Some(Errno::EINVAL), "zero inside the name");
        assert_eq!(with_name(b"\xff\xfe\0", 3), Some(Errno::EINVAL), "not UTF-8");
        assert_eq!(with_name(b"x\0", PATH_MAX as u32 + 1), Some(Errno::EINVAL), "name size over PATH_MAX");
    }

    #[test]
    fn rejects_data_past_the_end() {
        let mut archive = sample();
        // the file size of the first entry (`etc`) becomes huge
        archive[6 + 6 * 8..6 + 7 * 8].copy_from_slice(b"7FFFFFFF");
        assert_eq!(mount(archive).err(), Some(Errno::EINVAL));
    }

    #[test]
    fn every_truncation_fails_cleanly() {
        let archive = sample();
        // the last 512 byte block holds the trailer and its padding
        // a cut right before the zero byte of the name still reads it as zero padding
        let trailer_end = archive.windows(TRAILER.len()).position(|w| w == TRAILER.as_bytes()).unwrap() + TRAILER.len();
        for len in 0..archive.len() {
            let result = mount(archive[..len].to_vec());
            // a cut inside the padding of the last block leaves the archive intact,
            // MemBlockDevice pads the partial block with zeros
            if len >= trailer_end {
                assert!(result.is_ok(), "cut at {len}");
            } else {
                assert_eq!(result.err(), Some(Errno::EINVAL), "cut at {len}");
            }
        }
    }

    /// xorshift64, a fixed sequence so a failure reproduces
    struct Random(u64);

    impl Random {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    /// Reads every file and directory, as a user of the file system would
    fn walk(inode: &dyn Inode, depth: usize) {
        assert!(depth < 64, "directory cycle");
        let stat = inode.stat();
        match stat.file_type {
            FileType::Regular => {
                assert_eq!(read_to_end(inode).unwrap().len() as u64, stat.size);
            }
            FileType::Directory => {
                for entry in list_dir(inode).unwrap() {
                    walk(&*inode.lookup(&entry.name).unwrap(), depth + 1);
                }
            }
        }
    }

    #[test]
    fn corrupted_archives_never_panic() {
        let archive = sample();
        let mut random = Random(0x0517_2026);
        for _ in 0..20_000 {
            let mut corrupted = archive.clone();
            for _ in 0..1 + random.next() % 4 {
                let position = (random.next() % corrupted.len() as u64) as usize;
                // header bytes are hex digits, mostly corrupt them with other hex digits
                corrupted[position] = match random.next() % 3 {
                    0 => b"0123456789ABCDEF"[(random.next() % 16) as usize],
                    1 => b'0',
                    _ => random.next() as u8,
                };
            }
            match mount(corrupted) {
                Ok(fs) => walk(&*fs.root(), 0),
                // the only answer to a broken archive
                Err(e) => assert_eq!(e, Errno::EINVAL),
            }
        }
    }

    /// Made by bsdcpio 3.5.3 (libarchive) on macOS:
    /// `cd testdata/tree && find . | sort | cpio -o -H newc`
    #[test]
    fn mounts_an_archive_made_by_cpio() {
        let archive = include_bytes!("../testdata/tree.cpio");
        let fs = mount(archive.to_vec()).unwrap();
        assert_eq!(read(&fs, "/etc/motd"), Ok(include_bytes!("../testdata/tree/etc/motd").to_vec()));
        assert_eq!(read(&fs, "/hello.txt"), Ok(include_bytes!("../testdata/tree/hello.txt").to_vec()));
        assert_eq!(names(&fs, "/"), ["etc", "hello.txt"]);
        assert_eq!(resolve(&fs.root(), "/etc").unwrap().stat().file_type, FileType::Directory);
        assert_eq!(fs.root().stat().ino, 1);
    }
}
