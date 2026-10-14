//! `eventfd` and `epoll`, the files Go's network poller needs
//!
//! Go creates an `eventfd` to wake its poller and an `epoll` to wait on it. A static
//! program that only prints and sleeps still sets these up, and its scheduler waits in
//! `epoll_pwait` with a timeout to drive timers. The state here is pure and host-tested;
//! the kernel blocks a thread and wakes it when an event arrives or the timeout passes.

use crate::errno::{Errno, KResult};
use crate::file::OpenDescription;
use crate::file::{FileLike, Ready};
use crate::sync::Spinlock;
use crate::vfs::{FileType, Stat};
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

/// `eventfd2` flags
pub const EFD_SEMAPHORE: u32 = 1;
pub const EFD_CLOEXEC: u32 = 0o2000000;
pub const EFD_NONBLOCK: u32 = 0o4000;

/// A counter a program reads and writes through an 8-byte value, and that `epoll` can watch
pub struct EventFd {
    count: Spinlock<u64>,
    semaphore: bool,
    read_epoch: AtomicU64,
    write_epoch: AtomicU64,
}

impl EventFd {
    pub fn new(initial: u64, flags: u32) -> EventFd {
        EventFd {
            count: Spinlock::new("EVENTFD", initial),
            semaphore: flags & EFD_SEMAPHORE != 0,
            read_epoch: AtomicU64::new(1),
            write_epoch: AtomicU64::new(1),
        }
    }
}

fn anon_stat() -> Stat {
    Stat {
        ino: 0,
        file_type: FileType::CharDevice,
        permissions: 0o600,
        size: 0,
        nlink: 1,
        rdev: 0,
    }
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
        if *count == u64::MAX - 1 {
            self.write_epoch.fetch_add(1, Ordering::Relaxed);
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
                if *count == 0 && sum != 0 {
                    self.read_epoch.fetch_add(1, Ordering::Relaxed);
                }
                *count = sum;
                Ok(8)
            }
            _ => Err(Errno::EAGAIN),
        }
    }

    fn poll(&self) -> Ready {
        let count = *self.count.lock();
        Ready {
            readable: count > 0,
            writable: count < u64::MAX - 1,
        }
    }

    fn readiness_epochs(&self) -> [u64; 2] {
        [
            self.read_epoch.load(Ordering::Relaxed),
            self.write_epoch.load(Ordering::Relaxed),
        ]
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

struct Registration {
    interest: Interest,
    target: Weak<OpenDescription>,
    delivered: [Option<u64>; 2],
    armed: bool,
}
/// An `epoll` instance: the set of descriptors it watches and with which events
pub struct Epoll {
    registrations: Spinlock<Vec<Registration>>,
}

impl Default for Epoll {
    fn default() -> Self {
        Epoll::new()
    }
}

impl Epoll {
    pub fn new() -> Epoll {
        Epoll {
            registrations: Spinlock::new("EPOLL_REGISTRATIONS", Vec::new()),
        }
    }

    /// Registrations identify the open description, not a subsequently reused fd.
    pub fn register(
        &self,
        op: u64,
        fd: i32,
        events: u32,
        data: u64,
        target: &Arc<OpenDescription>,
    ) -> KResult<()> {
        const SUPPORTED: u32 = 1 | 4 | 8 | 0x10 | 0x2000 | (1 << 30) | (1 << 31);
        if events & !SUPPORTED != 0 {
            return Err(Errno::EINVAL);
        }
        let mut registrations = self.registrations.lock();
        registrations.retain(|r| r.target.strong_count() > 0);
        let index = registrations
            .iter()
            .position(|r| r.interest.fd == fd && r.target.ptr_eq(&Arc::downgrade(target)));
        let entry = || Registration {
            interest: Interest { fd, events, data },
            target: Arc::downgrade(target),
            delivered: [None, None],
            armed: true,
        };
        match op {
            EPOLL_CTL_ADD if index.is_some() => Err(Errno::EEXIST),
            EPOLL_CTL_ADD => {
                registrations.push(entry());
                Ok(())
            }
            EPOLL_CTL_MOD => {
                registrations[index.ok_or(Errno::ENOENT)?] = entry();
                Ok(())
            }
            EPOLL_CTL_DEL => {
                registrations.remove(index.ok_or(Errno::ENOENT)?);
                Ok(())
            }
            _ => Err(Errno::EINVAL),
        }
    }
    /// Level, edge and one-shot delivery, with errors and hangups always reported.
    pub fn scan(&self, max: usize) -> Vec<(u64, u32)> {
        let mut result = Vec::new();
        let mut registrations = self.registrations.lock();
        registrations.retain(|r| r.target.strong_count() > 0);
        let mut last = 0;
        for (index, r) in registrations.iter_mut().enumerate() {
            if result.len() >= max {
                break;
            }
            if !r.armed {
                continue;
            }
            let Some(target) = r.target.upgrade() else {
                continue;
            };
            let flags = target.file().poll_flags() & (r.interest.events | 8 | 0x10);
            let epochs = target.file().readiness_epochs();
            let mut deliver = flags;
            if r.interest.events & (1 << 31) != 0 {
                deliver = 0;
                for (i, mask) in [1 | 8 | 0x10 | 0x2000, 4 | 8 | 0x10]
                    .into_iter()
                    .enumerate()
                {
                    if flags & mask == 0 {
                        r.delivered[i] = None;
                    } else if r.delivered[i] != Some(epochs[i]) {
                        deliver |= flags & mask;
                        r.delivered[i] = Some(epochs[i]);
                    }
                }
            }
            if deliver != 0 {
                result.push((r.interest.data, deliver));
                last = index + 1;
                if r.interest.events & (1 << 30) != 0 {
                    r.armed = false;
                }
            }
        }
        if !result.is_empty() && last < registrations.len() {
            registrations.rotate_left(last);
        }
        result
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
        assert_eq!(
            eventfd.poll(),
            Ready {
                readable: false,
                writable: true
            }
        );
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
        assert_eq!(
            eventfd.read(0, &mut buf, true),
            Err(Errno::EAGAIN),
            "drained one at a time"
        );
    }

    #[test]
    fn eventfd_rejects_the_max() {
        let eventfd = EventFd::new(0, 0);
        assert_eq!(
            eventfd.write(0, &u64::MAX.to_le_bytes()),
            Err(Errno::EINVAL)
        );
        eventfd.write(0, &(u64::MAX - 2).to_le_bytes()).unwrap();
        assert_eq!(
            eventfd.write(0, &5u64.to_le_bytes()),
            Err(Errno::EAGAIN),
            "would overflow"
        );
    }

    fn description(file: alloc::sync::Arc<dyn FileLike>) -> Arc<OpenDescription> {
        Arc::new(OpenDescription::new(
            file,
            crate::file::Access {
                read: true,
                write: true,
            },
            0,
        ))
    }
    #[test]
    fn epoll_edges_rearm_after_drain_without_an_intervening_scan() {
        let event = Arc::new(EventFd::new(0, 0));
        let target = description(event.clone());
        let epoll = Epoll::new();
        assert_eq!(
            epoll.register(EPOLL_CTL_ADD, 5, 1 | (1 << 31), 0xaa, &target),
            Ok(())
        );
        assert_eq!(
            epoll.register(EPOLL_CTL_ADD, 5, 1, 0xaa, &target),
            Err(Errno::EEXIST)
        );
        assert!(epoll.scan(4).is_empty());
        event.write(0, &1u64.to_le_bytes()).unwrap();
        assert_eq!(epoll.scan(4), [(0xaa, 1)]);
        assert!(epoll.scan(4).is_empty());
        event.read(0, &mut [0; 8], true).unwrap();
        event.write(0, &1u64.to_le_bytes()).unwrap();
        assert_eq!(epoll.scan(4), [(0xaa, 1)]);
        assert_eq!(epoll.register(EPOLL_CTL_DEL, 5, 0, 0, &target), Ok(()));
        assert_eq!(
            epoll.register(EPOLL_CTL_DEL, 5, 0, 0, &target),
            Err(Errno::ENOENT)
        );
    }
    #[test]
    fn epoll_one_shot_level_and_closed_descriptor_reuse() {
        let event = Arc::new(EventFd::new(1, 0));
        let target = description(event.clone());
        let epoll = Epoll::new();
        epoll
            .register(EPOLL_CTL_ADD, 5, 1 | (1 << 30), 1, &target)
            .unwrap();
        assert_eq!(epoll.scan(1), [(1, 1)]);
        assert!(epoll.scan(1).is_empty());
        epoll.register(EPOLL_CTL_MOD, 5, 1, 2, &target).unwrap();
        assert_eq!(epoll.scan(1), [(2, 1)]);
        assert_eq!(epoll.scan(1), [(2, 1)]);
        drop(target);
        let replacement = description(Arc::new(EventFd::new(0, 0)));
        assert!(epoll.scan(1).is_empty());
        assert_eq!(
            epoll.register(EPOLL_CTL_MOD, 5, 1, 3, &replacement),
            Err(Errno::ENOENT)
        );
        epoll
            .register(EPOLL_CTL_ADD, 5, 1, 3, &replacement)
            .unwrap();
        assert!(epoll.scan(1).is_empty());
        assert_eq!(
            epoll.register(EPOLL_CTL_ADD, 6, 2, 0, &replacement),
            Err(Errno::EINVAL)
        );
    }
    #[test]
    fn epoll_limited_scan_rotates_ready_files() {
        let a = description(Arc::new(EventFd::new(1, 0)));
        let b = description(Arc::new(EventFd::new(1, 0)));
        let epoll = Epoll::new();
        epoll.register(EPOLL_CTL_ADD, 3, 1, 1, &a).unwrap();
        epoll.register(EPOLL_CTL_ADD, 4, 1, 2, &b).unwrap();
        assert_eq!(epoll.scan(1), [(1, 1)]);
        assert_eq!(epoll.scan(1), [(2, 1)]);
    }
}
