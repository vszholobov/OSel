//! The virtual file system interface: what every file system provides and how
//! a path is resolved
//!
//! A file system (`FileSystem`) is a tree of `Inode`s. Directories answer
//! `lookup` and `read_dir`, files answer `read_at`; whatever a node can't do
//! fails with the Linux error a program would get. Nothing here knows about a
//! concrete format: `cpio` is one implementation, a disk file system plugs in
//! the same way.

use crate::errno::{Errno, KResult};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

/// Longest path accepted, `PATH_MAX` of Linux including the terminating zero
pub const PATH_MAX: usize = 4096;
/// Longest name of one path component, `NAME_MAX` of Linux
pub const NAME_MAX: usize = 255;

/// What kind of node an inode is. More kinds (character devices, symlinks) come
/// with their file systems, so code outside `osel-core` keeps a fallback arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum FileType {
    #[default]
    Regular,
    Directory,
    /// A device read and written byte by byte, like `/dev/tty`
    CharDevice,
    /// An anonymous byte-stream pipe
    Fifo,
    /// A network socket exposed through a file descriptor
    Socket,
}

impl FileType {
    /// The `S_IFMT` bits of `st_mode` for this type
    pub const fn mode_bits(self) -> u32 {
        match self {
            FileType::Regular => 0o100000,
            FileType::Directory => 0o040000,
            FileType::CharDevice => 0o020000,
            FileType::Fifo => 0o010000,
            FileType::Socket => 0o140000,
        }
    }
}

/// What `stat` reports about an inode. Build it with `..Stat::default()`: fields
/// for `fstat` (links, device, times) will be added as they are needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stat {
    /// Inode number, unique within its file system
    pub ino: u64,
    pub file_type: FileType,
    /// Permission bits, `0o755` and alike
    pub permissions: u32,
    /// Size in bytes; for directories whatever the file system reports
    pub size: u64,
    /// Number of names of the node: 0 means unknown, `fstat` then reports 1
    pub nlink: u64,
    /// Device number of a device node, `major << 8 | minor`
    pub rdev: u64,
}

impl Stat {
    /// `st_mode` as Linux reports it: type and permission bits
    pub const fn mode(&self) -> u32 {
        self.file_type.mode_bits() | self.permissions
    }
}

/// One entry of a directory listing
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub ino: u64,
    pub file_type: FileType,
}

/// A node of a file system: a file or a directory
pub trait Inode: Send + Sync {
    fn stat(&self) -> Stat;

    /// Reads at byte `offset` into `buf`, returns how many bytes were read;
    /// 0 means the end of the file
    fn read_at(&self, _offset: u64, _buf: &mut [u8]) -> KResult<usize> {
        Err(if self.stat().file_type == FileType::Directory { Errno::EISDIR } else { Errno::EINVAL })
    }

    /// Finds the child `name` of a directory; `name` is never `.`, `..` or empty
    fn lookup(&self, _name: &str) -> KResult<Arc<dyn Inode>> {
        Err(Errno::ENOTDIR)
    }

    /// The directory that contains this one; the root of a file system is its own parent
    fn parent(&self) -> KResult<Arc<dyn Inode>> {
        Err(Errno::ENOTDIR)
    }

    /// The directory entry at `cursor` and the cursor of the next one, `None` past
    /// the last entry. Listing starts at cursor 0; other values are only those the
    /// directory returned, like `d_off` of `getdents64`. `.` and `..` are not listed.
    fn read_dir(&self, _cursor: u64) -> KResult<Option<(DirEntry, u64)>> {
        Err(Errno::ENOTDIR)
    }
}

/// A mounted file system
pub trait FileSystem: Send + Sync {
    /// The top directory
    fn root(&self) -> Arc<dyn Inode>;

    /// Short name of the format, as in `/proc/mounts`
    fn name(&self) -> &'static str;
}

/// Finds the inode at `path`: from `root` if the path starts with `/`, otherwise
/// from the directory `cwd`. Like Linux path resolution: repeated `/` and `.` are
/// skipped, `..` goes to the parent and stays at the root, `.` or `..` after a file
/// and a trailing `/` after a file are `ENOTDIR`.
pub fn resolve(root: &Arc<dyn Inode>, cwd: &Arc<dyn Inode>, path: &str) -> KResult<Arc<dyn Inode>> {
    if path.is_empty() {
        return Err(Errno::ENOENT);
    }
    if path.len() >= PATH_MAX {
        return Err(Errno::ENAMETOOLONG);
    }
    let root_ino = root.stat().ino;
    let mut current = if path.starts_with('/') { root.clone() } else { cwd.clone() };
    for name in path.split('/') {
        if name.is_empty() {
            continue;
        }
        // a file in the middle of a path fails before its name is even looked at
        if current.stat().file_type != FileType::Directory {
            return Err(Errno::ENOTDIR);
        }
        current = match name {
            "." => current,
            // `..` never leaves `root`, even if it is not the root of its file system
            ".." if current.stat().ino == root_ino => current,
            // a file system's own root may be wrapped (see `devices`): keep the wrapper
            ".." => match current.parent()? {
                parent if parent.stat().ino == root_ino => root.clone(),
                parent => parent,
            },
            name if name.len() > NAME_MAX => return Err(Errno::ENAMETOOLONG),
            name => current.lookup(name)?,
        };
    }
    if path.ends_with('/') && current.stat().file_type != FileType::Directory {
        return Err(Errno::ENOTDIR);
    }
    Ok(current)
}

/// Finds the inode at the absolute `path`
pub fn resolve_absolute(root: &Arc<dyn Inode>, path: &str) -> KResult<Arc<dyn Inode>> {
    resolve(root, root, path)
}

/// Reads a whole file; `ENOMEM` if it doesn't fit into the heap
pub fn read_to_end(inode: &dyn Inode) -> KResult<Vec<u8>> {
    let stat = inode.stat();
    if stat.file_type == FileType::Directory {
        return Err(Errno::EISDIR);
    }
    let size = usize::try_from(stat.size).map_err(|_| Errno::ENOMEM)?;
    let mut data = Vec::new();
    // a big file must fail the read, not the whole kernel
    data.try_reserve_exact(size).map_err(|_| Errno::ENOMEM)?;
    data.resize(size, 0);
    let mut done = 0;
    while done < data.len() {
        let read = inode.read_at(done as u64, &mut data[done..])?;
        if read == 0 {
            break;
        }
        done += read;
    }
    data.truncate(done);
    Ok(data)
}

/// All entries of a directory
pub fn list_dir(inode: &dyn Inode) -> KResult<Vec<DirEntry>> {
    let mut entries = Vec::new();
    let mut cursor = 0;
    while let Some((entry, next)) = inode.read_dir(cursor)? {
        entries.push(entry);
        cursor = next;
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::borrow::ToOwned;

    /// A file system built right in the test: nodes in an array, linked by index
    struct Tree(Vec<(u64, usize, Option<Vec<u8>>, Vec<(String, usize)>)>);

    /// `/`(1): `etc`(2): `motd`(3) = "hi", `bin`(4): empty
    fn tree() -> Arc<dyn Inode> {
        let dir = |ino, parent, children: &[(&str, usize)]| {
            (ino, parent, None, children.iter().map(|(name, i)| ((*name).to_owned(), *i)).collect())
        };
        let tree = Tree(vec![
            dir(1, 0, &[("bin", 3), ("etc", 1)]),
            dir(2, 0, &[("motd", 2)]),
            (3, 1, Some(b"hi".to_vec()), Vec::new()),
            dir(4, 0, &[]),
        ]);
        Arc::new(TestInode(Arc::new(tree), 0))
    }

    struct TestInode(Arc<Tree>, usize);

    impl TestInode {
        fn node(&self) -> &(u64, usize, Option<Vec<u8>>, Vec<(String, usize)>) {
            &self.0.0[self.1]
        }
        fn at(&self, index: usize) -> Arc<dyn Inode> {
            Arc::new(TestInode(self.0.clone(), index))
        }
    }

    impl Inode for TestInode {
        fn stat(&self) -> Stat {
            let (ino, _, data, _) = self.node();
            match data {
                Some(data) => Stat {
                    ino: *ino,
                    file_type: FileType::Regular,
                    permissions: 0o644,
                    size: data.len() as u64,
                    ..Stat::default()
                },
                None => Stat {
                    ino: *ino,
                    file_type: FileType::Directory,
                    permissions: 0o755,
                    ..Stat::default()
                },
            }
        }

        fn read_at(&self, offset: u64, buf: &mut [u8]) -> KResult<usize> {
            let Some(data) = &self.node().2 else {
                return Err(Errno::EISDIR);
            };
            let start = (offset as usize).min(data.len());
            let len = (data.len() - start).min(buf.len());
            buf[..len].copy_from_slice(&data[start..start + len]);
            Ok(len)
        }

        fn lookup(&self, name: &str) -> KResult<Arc<dyn Inode>> {
            let (_, _, data, children) = self.node();
            if data.is_some() {
                return Err(Errno::ENOTDIR);
            }
            let &(_, index) = children.iter().find(|(child, _)| child == name).ok_or(Errno::ENOENT)?;
            Ok(self.at(index))
        }

        fn parent(&self) -> KResult<Arc<dyn Inode>> {
            Ok(self.at(self.node().1))
        }

        fn read_dir(&self, cursor: u64) -> KResult<Option<(DirEntry, u64)>> {
            let (_, _, data, children) = self.node();
            if data.is_some() {
                return Err(Errno::ENOTDIR);
            }
            Ok(children.get(cursor as usize).map(|(name, index)| {
                let stat = self.at(*index).stat();
                (DirEntry { name: name.clone(), ino: stat.ino, file_type: stat.file_type }, cursor + 1)
            }))
        }
    }

    fn ino(root: &Arc<dyn Inode>, path: &str) -> KResult<u64> {
        resolve_absolute(root, path).map(|inode| inode.stat().ino)
    }

    #[test]
    fn resolves_absolute_paths() {
        let root = tree();
        assert_eq!(ino(&root, "/"), Ok(1));
        assert_eq!(ino(&root, "/etc"), Ok(2));
        assert_eq!(ino(&root, "/etc/motd"), Ok(3));
    }

    #[test]
    fn relative_paths_start_at_cwd() {
        let root = tree();
        let etc = resolve_absolute(&root, "/etc").unwrap();
        let at = |path| resolve(&root, &etc, path).map(|inode| inode.stat().ino);
        assert_eq!(at("motd"), Ok(3));
        assert_eq!(at("./motd"), Ok(3));
        assert_eq!(at("."), Ok(2));
        assert_eq!(at(".."), Ok(1), "`..` goes above the starting directory");
        assert_eq!(at("../bin"), Ok(4));
        assert_eq!(at("../../../bin"), Ok(4));
        assert_eq!(at("/etc/motd"), Ok(3), "an absolute path ignores cwd");
        assert_eq!(at("bin"), Err(Errno::ENOENT));
    }

    #[test]
    fn dot_dot_never_leaves_the_given_root() {
        // like chroot: a subdirectory as the root stops `..`
        let root = tree();
        let etc = resolve_absolute(&root, "/etc").unwrap();
        assert_eq!(ino(&etc, "/.."), Ok(2));
        assert_eq!(ino(&etc, "/../motd"), Ok(3));
    }

    #[test]
    fn skips_dots_and_repeated_slashes() {
        let root = tree();
        assert_eq!(ino(&root, "//etc///motd"), Ok(3));
        assert_eq!(ino(&root, "/./etc/./motd"), Ok(3));
        assert_eq!(ino(&root, "/etc/../bin"), Ok(4));
        assert_eq!(ino(&root, "/.."), Ok(1));
        assert_eq!(ino(&root, "/../../etc/motd"), Ok(3));
    }

    #[test]
    fn trailing_slash_and_dots_need_a_directory() {
        let root = tree();
        assert_eq!(ino(&root, "/etc/"), Ok(2));
        assert_eq!(ino(&root, "/etc/."), Ok(2));
        assert_eq!(ino(&root, "/etc/motd/"), Err(Errno::ENOTDIR));
        assert_eq!(ino(&root, "/etc/motd/."), Err(Errno::ENOTDIR));
        assert_eq!(ino(&root, "/etc/motd/.."), Err(Errno::ENOTDIR));
    }

    #[test]
    fn errors_like_linux() {
        let root = tree();
        assert_eq!(ino(&root, ""), Err(Errno::ENOENT));
        assert_eq!(ino(&root, "/nope"), Err(Errno::ENOENT));
        assert_eq!(ino(&root, "/etc/nope"), Err(Errno::ENOENT));
        assert_eq!(ino(&root, "/etc/motd/x"), Err(Errno::ENOTDIR));
        // Linux checks the directory before the name
        assert_eq!(ino(&root, &std::format!("/etc/motd/{}", "x".repeat(NAME_MAX + 1))), Err(Errno::ENOTDIR));
    }

    #[test]
    fn name_and_path_limits() {
        let root = tree();
        let long_name = "a".repeat(NAME_MAX + 1);
        assert_eq!(ino(&root, &long_name), Err(Errno::ENAMETOOLONG));
        // a name of exactly NAME_MAX is fine, it just doesn't exist
        assert_eq!(ino(&root, &"a".repeat(NAME_MAX)), Err(Errno::ENOENT));
        let long_path = "/.".repeat(PATH_MAX / 2);
        assert_eq!(ino(&root, &long_path), Err(Errno::ENAMETOOLONG));
        assert_eq!(ino(&root, &long_path[..PATH_MAX - 2]), Ok(1));
    }

    #[test]
    fn reads_whole_files() {
        let root = tree();
        assert_eq!(read_to_end(&*resolve_absolute(&root, "/etc/motd").unwrap()), Ok(b"hi".to_vec()));
        assert_eq!(read_to_end(&*root), Err(Errno::EISDIR));
    }

    /// A file that claims to be bigger than any memory
    struct Huge;

    impl Inode for Huge {
        fn stat(&self) -> Stat {
            Stat { size: u64::MAX / 2, ..Stat::default() }
        }
    }

    #[test]
    fn file_bigger_than_memory_is_enomem() {
        assert_eq!(read_to_end(&Huge), Err(Errno::ENOMEM));
    }

    #[test]
    fn lists_directories() {
        let root = tree();
        let names: Vec<String> = list_dir(&*root).unwrap().into_iter().map(|entry| entry.name).collect();
        assert_eq!(names, ["bin", "etc"]);
        assert_eq!(list_dir(&*resolve_absolute(&root, "/bin").unwrap()), Ok(Vec::new()));
        assert_eq!(list_dir(&*resolve_absolute(&root, "/etc/motd").unwrap()), Err(Errno::ENOTDIR));
    }

    /// A node that only implements `stat`: the defaults answer like Linux
    struct Bare(FileType);

    impl Inode for Bare {
        fn stat(&self) -> Stat {
            Stat { ino: 1, file_type: self.0, ..Stat::default() }
        }
    }

    #[test]
    fn default_methods_fail_with_the_right_error() {
        let file = Bare(FileType::Regular);
        assert_eq!(file.lookup("x").err(), Some(Errno::ENOTDIR));
        assert_eq!(file.parent().err(), Some(Errno::ENOTDIR));
        assert_eq!(file.read_dir(0), Err(Errno::ENOTDIR));
        assert_eq!(file.read_at(0, &mut [0; 1]), Err(Errno::EINVAL));
        assert_eq!(Bare(FileType::Directory).read_at(0, &mut [0; 1]), Err(Errno::EISDIR));
    }

    #[test]
    fn mode_combines_type_and_permissions() {
        let stat = Stat { ino: 1, file_type: FileType::Directory, permissions: 0o755, size: 0, ..Stat::default() };
        assert_eq!(stat.mode(), 0o40755);
        let stat = Stat { file_type: FileType::Regular, permissions: 0o644, ..stat };
        assert_eq!(stat.mode(), 0o100644);
        let socket = Stat { file_type: FileType::Socket, permissions: 0o600, ..stat };
        assert_eq!(socket.mode(), 0o140600);
    }
}
