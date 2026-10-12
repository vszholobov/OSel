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
use alloc::vec;
use alloc::vec::Vec;

/// Longest path accepted, `PATH_MAX` of Linux including the terminating zero
pub const PATH_MAX: usize = 4096;
/// Longest name of one path component, `NAME_MAX` of Linux
pub const NAME_MAX: usize = 255;

/// What kind of node an inode is
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    Regular,
    Directory,
}

impl FileType {
    /// The `S_IFMT` bits of `st_mode` for this type
    pub const fn mode_bits(self) -> u32 {
        match self {
            FileType::Regular => 0o100000,
            FileType::Directory => 0o040000,
        }
    }
}

/// What `stat` reports about an inode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    /// Inode number, unique within its file system
    pub ino: u64,
    pub file_type: FileType,
    /// Permission bits, `0o755` and alike
    pub permissions: u32,
    /// Size in bytes; for directories whatever the file system reports
    pub size: u64,
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
        Err(match self.stat().file_type {
            FileType::Directory => Errno::EISDIR,
            FileType::Regular => Errno::EINVAL,
        })
    }

    /// Finds the child `name` of a directory; `name` is never `.`, `..` or empty
    fn lookup(&self, _name: &str) -> KResult<Arc<dyn Inode>> {
        Err(Errno::ENOTDIR)
    }

    /// The entry number `index` of a directory, `None` past the last one.
    /// `.` and `..` are not listed.
    fn read_dir(&self, _index: usize) -> KResult<Option<DirEntry>> {
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

/// Finds the inode at `path`, starting from `root`. The path is taken as absolute:
/// a leading `/` is optional. `.` and repeated slashes are skipped, `..` goes up
/// and stays at the root. A trailing slash requires a directory.
pub fn resolve(root: &Arc<dyn Inode>, path: &str) -> KResult<Arc<dyn Inode>> {
    if path.is_empty() {
        return Err(Errno::ENOENT);
    }
    if path.len() >= PATH_MAX {
        return Err(Errno::ENAMETOOLONG);
    }
    let is_directory = |node: &Arc<dyn Inode>| node.stat().file_type == FileType::Directory;
    // ancestors of the current node, for `..`
    let mut stack: Vec<Arc<dyn Inode>> = vec![root.clone()];
    for name in path.split('/') {
        let current = stack.last().expect("the root is never popped");
        match name {
            "" => {}
            // like Linux, `file/.` and `file/..` are ENOTDIR, not the file or its parent
            "." | ".." if !is_directory(current) => return Err(Errno::ENOTDIR),
            "." => {}
            ".." => {
                if stack.len() > 1 {
                    stack.pop();
                }
            }
            name => {
                if name.len() > NAME_MAX {
                    return Err(Errno::ENAMETOOLONG);
                }
                let child = current.lookup(name)?;
                stack.push(child);
            }
        }
    }
    let node = stack.pop().expect("the root is never popped");
    if path.ends_with('/') && !is_directory(&node) {
        return Err(Errno::ENOTDIR);
    }
    Ok(node)
}

/// Reads a whole file
pub fn read_to_end(inode: &dyn Inode) -> KResult<Vec<u8>> {
    let stat = inode.stat();
    if stat.file_type == FileType::Directory {
        return Err(Errno::EISDIR);
    }
    let mut data = vec![0u8; usize::try_from(stat.size).map_err(|_| Errno::ENOMEM)?];
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
    while let Some(entry) = inode.read_dir(entries.len())? {
        entries.push(entry);
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::borrow::ToOwned;
    use std::collections::BTreeMap;

    /// A file system built right in the test: maps of names to nodes
    enum Node {
        File(u64, Vec<u8>),
        Dir(u64, BTreeMap<String, Arc<Node>>),
    }

    impl Inode for Node {
        fn stat(&self) -> Stat {
            match self {
                Node::File(ino, data) => {
                    Stat { ino: *ino, file_type: FileType::Regular, permissions: 0o644, size: data.len() as u64 }
                }
                Node::Dir(ino, _) => Stat { ino: *ino, file_type: FileType::Directory, permissions: 0o755, size: 0 },
            }
        }

        fn read_at(&self, offset: u64, buf: &mut [u8]) -> KResult<usize> {
            let Node::File(_, data) = self else { return Err(Errno::EISDIR) };
            let start = (offset as usize).min(data.len());
            let len = (data.len() - start).min(buf.len());
            buf[..len].copy_from_slice(&data[start..start + len]);
            Ok(len)
        }

        fn lookup(&self, name: &str) -> KResult<Arc<dyn Inode>> {
            let Node::Dir(_, children) = self else { return Err(Errno::ENOTDIR) };
            let child: Arc<dyn Inode> = children.get(name).ok_or(Errno::ENOENT)?.clone();
            Ok(child)
        }

        fn read_dir(&self, index: usize) -> KResult<Option<DirEntry>> {
            let Node::Dir(_, children) = self else { return Err(Errno::ENOTDIR) };
            Ok(children.iter().nth(index).map(|(name, node)| {
                let stat = node.stat();
                DirEntry { name: name.clone(), ino: stat.ino, file_type: stat.file_type }
            }))
        }
    }

    /// `/`(1): `etc`(2): `motd`(3) = "hi", `bin`(4): empty
    fn tree() -> Arc<dyn Inode> {
        let motd = Arc::new(Node::File(3, b"hi".to_vec()));
        let etc = Arc::new(Node::Dir(2, BTreeMap::from([("motd".to_owned(), motd)])));
        let bin = Arc::new(Node::Dir(4, BTreeMap::new()));
        Arc::new(Node::Dir(1, BTreeMap::from([("etc".to_owned(), etc), ("bin".to_owned(), bin)])))
    }

    fn ino(root: &Arc<dyn Inode>, path: &str) -> KResult<u64> {
        resolve(root, path).map(|inode| inode.stat().ino)
    }

    #[test]
    fn resolves_absolute_and_relative_paths() {
        let root = tree();
        assert_eq!(ino(&root, "/"), Ok(1));
        assert_eq!(ino(&root, "/etc"), Ok(2));
        assert_eq!(ino(&root, "/etc/motd"), Ok(3));
        assert_eq!(ino(&root, "etc/motd"), Ok(3));
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
        assert_eq!(read_to_end(&*resolve(&root, "/etc/motd").unwrap()), Ok(b"hi".to_vec()));
        assert_eq!(read_to_end(&*root), Err(Errno::EISDIR));
    }

    #[test]
    fn lists_directories() {
        let root = tree();
        let names: Vec<String> = list_dir(&*root).unwrap().into_iter().map(|entry| entry.name).collect();
        assert_eq!(names, ["bin", "etc"]);
        assert_eq!(list_dir(&*resolve(&root, "/bin").unwrap()), Ok(Vec::new()));
        assert_eq!(list_dir(&*resolve(&root, "/etc/motd").unwrap()), Err(Errno::ENOTDIR));
    }

    /// A node that only implements `stat`: the defaults answer like Linux
    struct Bare(FileType);

    impl Inode for Bare {
        fn stat(&self) -> Stat {
            Stat { ino: 1, file_type: self.0, permissions: 0, size: 0 }
        }
    }

    #[test]
    fn default_methods_fail_with_the_right_error() {
        let file = Bare(FileType::Regular);
        assert_eq!(file.lookup("x").err(), Some(Errno::ENOTDIR));
        assert_eq!(file.read_dir(0), Err(Errno::ENOTDIR));
        assert_eq!(file.read_at(0, &mut [0; 1]), Err(Errno::EINVAL));
        assert_eq!(Bare(FileType::Directory).read_at(0, &mut [0; 1]), Err(Errno::EISDIR));
    }

    #[test]
    fn mode_combines_type_and_permissions() {
        let stat = Stat { ino: 1, file_type: FileType::Directory, permissions: 0o755, size: 0 };
        assert_eq!(stat.mode(), 0o40755);
        let stat = Stat { file_type: FileType::Regular, permissions: 0o644, ..stat };
        assert_eq!(stat.mode(), 0o100644);
    }
}
