//! Anonymous pipes: bounded byte streams and scheduler-backed blocking I/O.

use crate::scheduler;
use crate::sync::IrqSpinlock;
use alloc::sync::Arc;
use alloc::vec::Vec;
use osel_core::errno::{Errno, KResult};
use osel_core::file::{Access, FileLike, O_CLOEXEC, O_NONBLOCK, OpenDescription, Ready};
use osel_core::pipe::Pipe;
use osel_core::sched::ThreadId;
use osel_core::vfs::{FileType, Stat};
use x86_64::instructions::interrupts as irq;

// One entry per blocked thread. A pipe's allocation address identifies it while an
// endpoint (including the blocked call's description) keeps that allocation alive.
static PIPE_WAITERS: IrqSpinlock<Vec<(usize, ThreadId)>> =
    IrqSpinlock::new("PIPE_WAITERS", Vec::new());

pub fn init(max_waiters: usize) {
    PIPE_WAITERS.lock().reserve(max_waiters);
}

pub fn forget(thread: ThreadId) {
    PIPE_WAITERS.lock().retain(|&(_, id)| id != thread);
}

fn notify(key: usize) {
    {
        let waiters = PIPE_WAITERS.lock();
        for &(pipe, id) in waiters.iter() {
            if pipe == key {
                scheduler::wake(id);
            }
        }
    }
    crate::poll::notify();
}

struct Endpoint {
    state: Arc<IrqSpinlock<Pipe>>,
    reader: bool,
}

impl Endpoint {
    fn key(&self) -> usize {
        Arc::as_ptr(&self.state) as usize
    }

    /// The check, registration and sleep form one non-preemptible operation. No pipe
    /// lock survives a context switch, and a close cannot be missed between them.
    fn operate(
        &self,
        nonblock: bool,
        mut operation: impl FnMut(&mut Pipe) -> KResult<usize>,
    ) -> KResult<usize> {
        loop {
            let result = irq::without_interrupts(|| {
                if scheduler::current_killed() {
                    return Err(Errno::EINTR);
                }
                let result = operation(&mut self.state.lock());
                if result == Err(Errno::EAGAIN) && !nonblock {
                    let id = scheduler::current_id();
                    PIPE_WAITERS.lock().push((self.key(), id));
                    scheduler::block_current();
                    forget(id);
                }
                result
            });
            if result != Err(Errno::EAGAIN) || nonblock {
                if result.is_ok() {
                    notify(self.key());
                }
                return result;
            }
        }
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        {
            let mut state = self.state.lock();
            if self.reader {
                state.close_reader();
            } else {
                state.close_writer();
            }
        }
        notify(self.key());
    }
}

impl FileLike for Endpoint {
    fn read(&self, _offset: u64, buf: &mut [u8], nonblock: bool) -> KResult<usize> {
        if !self.reader {
            return Err(Errno::EBADF);
        }
        self.operate(nonblock, |pipe| pipe.read(buf))
    }

    fn write_with_flags(&self, _offset: u64, buf: &[u8], nonblock: bool) -> KResult<usize> {
        if self.reader {
            return Err(Errno::EBADF);
        }
        self.operate(nonblock, |pipe| pipe.write(buf))
    }

    fn stat(&self) -> Stat {
        Stat {
            file_type: FileType::Fifo,
            permissions: 0o600,
            ..Stat::default()
        }
    }

    fn poll(&self) -> Ready {
        let state = self.state.lock();
        if self.reader {
            state.read_ready()
        } else {
            state.write_ready()
        }
    }

    fn readiness_epochs(&self) -> [u64; 2] { self.state.lock().readiness_epochs() }
    fn poll_flags(&self) -> u32 { let state = self.state.lock(); if self.reader { state.read_flags() } else { state.write_flags() } }
    fn epollable(&self) -> bool {
        true
    }
}

/// Creates the read and write descriptions of `pipe2`. Descriptor insertion and
/// rollback on a full table belong to the syscall layer.
pub fn create(flags: u32) -> KResult<(Arc<OpenDescription>, Arc<OpenDescription>)> {
    if flags & !(O_CLOEXEC | O_NONBLOCK) != 0 {
        return Err(Errno::EINVAL);
    }
    let state = Arc::new(IrqSpinlock::new("PIPE", Pipe::new()));
    let read = Arc::new(OpenDescription::new(
        Arc::new(Endpoint {
            state: state.clone(),
            reader: true,
        }),
        Access {
            read: true,
            write: false,
        },
        flags,
    ));
    let write = Arc::new(OpenDescription::new(
        Arc::new(Endpoint {
            state,
            reader: false,
        }),
        Access {
            read: false,
            write: true,
        },
        flags,
    ));
    Ok((read, write))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use osel_core::file::FdTable;
    use osel_core::pipe::PIPE_CAPACITY;

    #[test_case]
    fn test_pipe_nonblocking_lifetime_and_flags() {
        let (read, write) = create(O_NONBLOCK).unwrap();
        assert_eq!(read.read(&mut [0]), Err(Errno::EAGAIN));
        assert_eq!(write.write(&vec![7; PIPE_CAPACITY]), Ok(PIPE_CAPACITY));
        assert_eq!(write.write(b"x"), Err(Errno::EAGAIN));
        let mut table = FdTable::new();
        let fd = table.insert(write, true).unwrap();
        table.dup(fd, 0, false).unwrap();
        table.close_on_exec();
        let mut buf = vec![0; PIPE_CAPACITY];
        assert_eq!(read.read(&mut buf), Ok(PIPE_CAPACITY));
        assert!(buf.iter().all(|&b| b == 7));
        assert_eq!(
            read.read(&mut [0]),
            Err(Errno::EAGAIN),
            "dup keeps the writer alive"
        );
        drop(table);
        assert_eq!(read.read(&mut [0]), Ok(0));
        let (read, write) = create(O_NONBLOCK).unwrap();
        drop(read);
        assert_eq!(write.write(b"x"), Err(Errno::EPIPE));
        assert_eq!(create(1).err(), Some(Errno::EINVAL));
    }

    static TEST_ENDPOINT: IrqSpinlock<Option<Arc<OpenDescription>>> =
        IrqSpinlock::new("PIPE_TEST", None);

    fn wait_thread(id: ThreadId) {
        let deadline = crate::interrupts::ticks() + 500;
        while scheduler::runs(id).is_some() && crate::interrupts::ticks() < deadline {
            scheduler::yield_now();
        }
        assert!(scheduler::runs(id).is_none(), "pipe worker did not finish");
    }

    #[test_case]
    fn test_pipe_reader_blocks_until_write_and_eof() {
        let (read, write) = create(0).unwrap();
        *TEST_ENDPOINT.lock() = Some(write);
        let worker = scheduler::spawn_kernel(|| {
            let write = TEST_ENDPOINT.lock().take().unwrap();
            scheduler::yield_now();
            assert_eq!(write.write(b"pipe"), Ok(4));
        })
        .unwrap();
        let mut buf = [0; 4];
        assert_eq!(read.read(&mut buf), Ok(4));
        assert_eq!(&buf, b"pipe");
        assert_eq!(read.read(&mut buf), Ok(0));
        wait_thread(worker);
    }

    #[test_case]
    fn test_pipe_full_writer_blocks_until_reader_frees_room() {
        let (read, write) = create(0).unwrap();
        assert_eq!(write.write(&vec![1; PIPE_CAPACITY]), Ok(PIPE_CAPACITY));
        *TEST_ENDPOINT.lock() = Some(read);
        let worker = scheduler::spawn_kernel(|| {
            let read = TEST_ENDPOINT.lock().take().unwrap();
            scheduler::yield_now();
            let mut buf = vec![0; PIPE_CAPACITY];
            assert_eq!(read.read(&mut buf), Ok(PIPE_CAPACITY));
            assert_eq!(read.read(&mut buf), Ok(1));
            assert_eq!(buf[0], 2);
        })
        .unwrap();
        assert_eq!(write.write(&[2]), Ok(1));
        drop(write);
        wait_thread(worker);
    }
}
