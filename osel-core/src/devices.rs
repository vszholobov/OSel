//! Device files: a `/dev` directory added to the root of a file system
//!
//! The ramdisk has no device nodes, and cpio device entries are not supported. Instead the
//! root directory is wrapped: its listing and `lookup` get a `dev` directory with one
//! node per device. A device node is an `Inode` of type `CharDevice` with a device
//! number; the kernel opens it as the device that number names.

use crate::errno::{Errno, KResult};
use crate::vfs::{DirEntry, FileSystem, FileType, Inode, Stat};
use alloc::string::String;
use alloc::sync::{Arc, Weak};

/// `major << 8 | minor`, as Linux reports `st_rdev` for small numbers
pub const fn device_number(major: u64, minor: u64) -> u64 {
    major << 8 | minor
}

/// `/dev/null`: Linux major 1, minor 3
pub const NULL: u64 = device_number(1, 3);
/// `/dev/tty`: Linux major 5, minor 0
pub const TTY: u64 = device_number(5, 0);

/// The devices under `/dev`: name and device number
pub const DEVICES: &[(&str, u64)] = &[("null", NULL), ("tty", TTY)];

/// Inode numbers of `/dev` and its nodes, far above anything the ramdisk numbers
const DEV_INO: u64 = 1 << 32;

/// A file system with `/dev` added to its root
pub struct WithDevices {
    root: Arc<Root>,
}

impl WithDevices {
    pub fn new(inner: Arc<dyn FileSystem>) -> WithDevices {
        let root = Arc::new_cyclic(|root: &Weak<Root>| {
            let dev = Arc::new(DevDir { parent: root.clone() });
            Root { inner: inner.root(), dev, name: inner.name() }
        });
        WithDevices { root }
    }
}

impl FileSystem for WithDevices {
    fn root(&self) -> Arc<dyn Inode> {
        self.root.clone()
    }

    fn name(&self) -> &'static str {
        self.root.name
    }
}

/// The wrapped root: everything of the original, plus `dev`
struct Root {
    inner: Arc<dyn Inode>,
    dev: Arc<DevDir>,
    name: &'static str,
}

impl Inode for Root {
    fn stat(&self) -> Stat {
        self.inner.stat()
    }

    fn lookup(&self, name: &str) -> KResult<Arc<dyn Inode>> {
        match name {
            "dev" => Ok(self.dev.clone()),
            name => self.inner.lookup(name),
        }
    }

    fn parent(&self) -> KResult<Arc<dyn Inode>> {
        // the root is its own parent; `resolve` keeps this wrapper
        self.inner.parent()
    }

    fn read_dir(&self, cursor: u64) -> KResult<Option<(DirEntry, u64)>> {
        // `dev` first, at cursor 0; the original listing follows with its cursors shifted
        if cursor == 0 {
            let entry = DirEntry { name: String::from("dev"), ino: DEV_INO, file_type: FileType::Directory };
            return Ok(Some((entry, 1)));
        }
        let Some((entry, next)) = self.inner.read_dir(cursor - 1)? else { return Ok(None) };
        // a `dev` of the ramdisk itself is hidden behind ours
        if entry.name == "dev" {
            return self.read_dir(next + 1);
        }
        Ok(Some((entry, next + 1)))
    }
}

/// `/dev`
struct DevDir {
    parent: Weak<Root>,
}

impl Inode for DevDir {
    fn stat(&self) -> Stat {
        Stat { ino: DEV_INO, file_type: FileType::Directory, permissions: 0o755, nlink: 2, ..Stat::default() }
    }

    fn lookup(&self, name: &str) -> KResult<Arc<dyn Inode>> {
        let index = DEVICES.iter().position(|(device, _)| *device == name).ok_or(Errno::ENOENT)?;
        Ok(Arc::new(DeviceNode { index }))
    }

    fn parent(&self) -> KResult<Arc<dyn Inode>> {
        Ok(self.parent.upgrade().ok_or(Errno::ENOENT)?)
    }

    fn read_dir(&self, cursor: u64) -> KResult<Option<(DirEntry, u64)>> {
        let Some((name, _)) = DEVICES.get(cursor as usize) else { return Ok(None) };
        let entry = DirEntry { name: String::from(*name), ino: DEV_INO + 1 + cursor, file_type: FileType::CharDevice };
        Ok(Some((entry, cursor + 1)))
    }
}

/// A node under `/dev`
struct DeviceNode {
    index: usize,
}

impl Inode for DeviceNode {
    fn stat(&self) -> Stat {
        node_stat(self.index)
    }
}

fn node_stat(index: usize) -> Stat {
    Stat {
        ino: DEV_INO + 1 + index as u64,
        file_type: FileType::CharDevice,
        permissions: 0o666,
        nlink: 1,
        rdev: DEVICES[index].1,
        ..Stat::default()
    }
}

/// What `stat` of the node of device `rdev` reports, also without a file system
pub fn device_stat(rdev: u64) -> Option<Stat> {
    DEVICES.iter().position(|(_, number)| *number == rdev).map(node_stat)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpio::{CpioBuilder, CpioFs};
    use crate::block::MemBlockDevice;
    use crate::vfs::{list_dir, read_to_end, resolve};
    use alloc::vec::Vec;

    fn fs() -> WithDevices {
        let mut builder = CpioBuilder::new();
        builder.add_dir("etc", 0o755).add_file("etc/motd", 0o644, b"hi\n");
        let cpio = CpioFs::mount(Arc::new(MemBlockDevice::new(builder.finish()))).unwrap();
        WithDevices::new(Arc::new(cpio))
    }

    #[test]
    fn devices_are_found_by_any_spelling_of_the_path() {
        let fs = fs();
        let root = fs.root();
        for path in ["/dev/null", "//dev/null", "/dev/./null", "/etc/../dev/null", "/dev/../dev/null"] {
            let node = resolve(&root, &root, path).unwrap_or_else(|e| panic!("{path}: {e}"));
            assert_eq!(node.stat().rdev, NULL, "{path}");
            assert_eq!(node.stat().file_type, FileType::CharDevice);
        }
        assert_eq!(resolve(&root, &root, "dev/tty").unwrap().stat().rdev, TTY, "relative to /");
        assert_eq!(resolve(&root, &root, "/dev/nope").err(), Some(Errno::ENOENT));
        assert_eq!(resolve(&root, &root, "/dev/null/x").err(), Some(Errno::ENOTDIR));
        assert_eq!(resolve(&root, &root, "/dev").unwrap().stat().file_type, FileType::Directory);
    }

    #[test]
    fn the_ramdisk_is_still_there() {
        let fs = fs();
        let root = fs.root();
        assert_eq!(read_to_end(&*resolve(&root, &root, "/etc/motd").unwrap()).unwrap(), b"hi\n");
        let names: Vec<String> = list_dir(&*root).unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["dev", "etc"]);
        let devices: Vec<String> = list_dir(&*resolve(&root, &root, "/dev").unwrap()).unwrap().into_iter().map(|e| e.name).collect();
        let expected: Vec<String> = DEVICES.iter().map(|(name, _)| String::from(*name)).collect();
        assert_eq!(devices, expected);
    }

    #[test]
    fn a_dev_of_the_ramdisk_is_hidden() {
        let mut builder = CpioBuilder::new();
        builder.add_dir("dev", 0o755).add_file("dev/mine", 0o644, b"x").add_dir("etc", 0o755);
        let cpio = CpioFs::mount(Arc::new(MemBlockDevice::new(builder.finish()))).unwrap();
        let fs = WithDevices::new(Arc::new(cpio));
        let root = fs.root();
        let names: Vec<String> = list_dir(&*root).unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["dev", "etc"], "one dev");
        assert_eq!(resolve(&root, &root, "/dev/mine").err(), Some(Errno::ENOENT));
    }

    #[test]
    fn device_numbers_are_linux_ones() {
        assert_eq!(NULL, 0x103);
        assert_eq!(TTY, 0x500);
        let root = fs().root();
        assert_eq!(device_stat(TTY), Some(resolve(&root, &root, "/dev/tty").unwrap().stat()));
        assert_eq!(device_stat(99), None);
    }
}
