//! `eventfd` and `epoll`, the files Go's network poller needs
//!
//! Go creates an `eventfd` to wake its poller and an `epoll` to wait on it. A static
//! program that only prints and sleeps still sets these up, and its scheduler waits in
//! `epoll_pwait` with a timeout to drive timers. The state here is pure and host-tested;
//! the kernel blocks a thread and wakes it when an event arrives or the timeout passes.

use crate::errno::{Errno, KResult};
use crate::file::{FileLike, Ready};
use crate::sync::Spinlock;
use crate::vfs::{FileType, Stat};
use alloc::vec::Vec;

/// `eventfd2` flags
pub const EFD_SEMAPHORE: u32 = 1;
pub const EFD_CLOEXEC: u32 = 0o2000000;
pub const EFD_NONBLOCK: u32 = 0o4000;

/// A counter a program reads and writes through an 8-byte value, and that `epoll` can watch
pub struct EventFd {
    count: Spinlock<u64>,
    semaphore: bool,
}

impl EventFd {
    pub fn new(initial: u64, flags: u32) -> EventFd {
        EventFd { count: Spinlock::new("EVENTFD", initial), semaphore: flags & EFD_SEMAPHORE != 0 }
    }
}

fn anon_stat() -> Stat {
    Stat { ino: 0, file_type: FileType::CharDevice, permissions: 0o600, size: 0, nlink: 1, rdev: 0 }
}

impl FileLike for EventFd {
    /// Reads the 8-byte counter: the whole value and zero it, or 1 in semaphore mode. A
    /// zero counter is `EAGAIN` (Go opens the eventfd non-blocking and never waits on it).
    fn read(&self, _offset: u64, buf: &mut [u8], _nonblock: bool) -> KResult<usize> {
        if buf.len() < 8 {
            return Err(Errno::EINVAL);
        }
        let mut count = self.count.lock();
        if *count == 0 {
            return Err(Errno::EAGAIN);
        }
        let value = if self.semaphore {
            *count -= 1;
            1
        } else {
            core::mem::take(&mut *count)
        };
        buf[..8].copy_from_slice(&value.to_le_bytes());
        Ok(8)
    }

    /// Adds an 8-byte value to the counter. `u64::MAX` is `EINVAL`; an overflow past the
    /// maximum is `EAGAIN` (no writer blocks here).
    fn write(&self, _offset: u64, buf: &[u8]) -> KResult<usize> {
        if buf.len() < 8 {
            return Err(Errno::EINVAL);
        }
        let value = u64::from_le_bytes(buf[..8].try_into().unwrap());
        if value == u64::MAX {
            return Err(Errno::EINVAL);
        }
        let mut count = self.count.lock();
        match count.checked_add(value) {
            Some(sum) if sum < u64::MAX => {
                *count = sum;
                Ok(8)
            }
            _ => Err(Errno::EAGAIN),
        }
    }

    fn poll(&self) -> Ready {
        let count = *self.count.lock();
        Ready { readable: count > 0, writable: count < u64::MAX - 1 }
    }

    fn epollable(&self) -> bool {
        true
    }

    fn stat(&self) -> Stat {
        anon_stat()
    }
}

/// One descriptor an `epoll` watches
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interest {
    pub fd: i32,
    pub events: u32,
    pub data: u64,
}

/// `epoll_ctl` operations
pub const EPOLL_CTL_ADD: u64 = 1;
pub const EPOLL_CTL_DEL: u64 = 2;
pub const EPOLL_CTL_MOD: u64 = 3;

/// An `epoll` instance: the set of descriptors it watches and with which events
pub struct Epoll {
    interest: Spinlock<Vec<Interest>>,
}

impl Default for Epoll {
    fn default() -> Self {
        Epoll::new()
    }
}

impl Epoll {
    pub fn new() -> Epoll {
        Epoll { interest: Spinlock::new("EPOLL", Vec::new()) }
    }

    /// `epoll_ctl`: add, change or remove a descriptor. `EEXIST` re-adding, `ENOENT`
    /// changing or removing one not watched, `EINVAL` for an unknown operation.
    pub fn ctl(&self, op: u64, fd: i32, events: u32, data: u64) -> KResult<()> {
        let mut interest = self.interest.lock();
        let found = interest.iter().position(|entry| entry.fd == fd);
        match op {
            EPOLL_CTL_ADD if found.is_some() => Err(Errno::EEXIST),
            EPOLL_CTL_ADD => {
                interest.push(Interest { fd, events, data });
                Ok(())
            }
            EPOLL_CTL_MOD => {
                let entry = found.ok_or(Errno::ENOENT)?;
                interest[entry] = Interest { fd, events, data };
                Ok(())
            }
            EPOLL_CTL_DEL => {
                let entry = found.ok_or(Errno::ENOENT)?;
                interest.remove(entry);
                Ok(())
            }
            _ => Err(Errno::EINVAL),
        }
    }

    /// The descriptors it watches, for the kernel to poll each
    pub fn watched(&self) -> Vec<Interest> {
        self.interest.lock().clone()
    }
}

impl FileLike for Epoll {
    fn stat(&self) -> Stat {
        anon_stat()
    }

    fn as_epoll(&self) -> Option<&Epoll> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eventfd_counts_up_and_drains() {
        let eventfd = EventFd::new(0, 0);
        assert_eq!(eventfd.poll(), Ready { readable: false, writable: true });
        let mut buf = [0u8; 8];
        assert_eq!(eventfd.read(0, &mut buf, true), Err(Errno::EAGAIN), "empty");
        assert_eq!(eventfd.write(0, &3u64.to_le_bytes()), Ok(8));
        assert_eq!(eventfd.write(0, &4u64.to_le_bytes()), Ok(8));
        assert!(eventfd.poll().readable);
        assert_eq!(eventfd.read(0, &mut buf, true), Ok(8));
        assert_eq!(u64::from_le_bytes(buf), 7, "the sum, then zero");
        assert_eq!(eventfd.read(0, &mut buf, true), Err(Errno::EAGAIN));
    }

    #[test]
    fn eventfd_semaphore_reads_one() {
        let eventfd = EventFd::new(2, EFD_SEMAPHORE);
        let mut buf = [0u8; 8];
        assert_eq!(eventfd.read(0, &mut buf, true), Ok(8));
        assert_eq!(u64::from_le_bytes(buf), 1);
        assert_eq!(eventfd.read(0, &mut buf, true), Ok(8));
        assert_eq!(eventfd.read(0, &mut buf, true), Err(Errno::EAGAIN), "drained one at a time");
    }

    #[test]
    fn eventfd_rejects_the_max() {
        let eventfd = EventFd::new(0, 0);
        assert_eq!(eventfd.write(0, &u64::MAX.to_le_bytes()), Err(Errno::EINVAL));
        eventfd.write(0, &(u64::MAX - 2).to_le_bytes()).unwrap();
        assert_eq!(eventfd.write(0, &5u64.to_le_bytes()), Err(Errno::EAGAIN), "would overflow");
    }

    #[test]
    fn epoll_tracks_interest() {
        let epoll = Epoll::new();
        assert_eq!(epoll.ctl(EPOLL_CTL_ADD, 5, 1, 0xaa), Ok(()));
        assert_eq!(epoll.ctl(EPOLL_CTL_ADD, 5, 1, 0xaa), Err(Errno::EEXIST));
        assert_eq!(epoll.ctl(EPOLL_CTL_MOD, 5, 3, 0xbb), Ok(()));
        assert_eq!(epoll.watched(), [Interest { fd: 5, events: 3, data: 0xbb }]);
        assert_eq!(epoll.ctl(EPOLL_CTL_DEL, 5, 0, 0), Ok(()));
        assert_eq!(epoll.ctl(EPOLL_CTL_DEL, 5, 0, 0), Err(Errno::ENOENT));
        assert_eq!(epoll.ctl(99, 5, 0, 0), Err(Errno::EINVAL));
    }
}
