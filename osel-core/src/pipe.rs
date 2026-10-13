//! Bounded anonymous pipe state, independent of threads and interrupts.

use crate::errno::{Errno, KResult};
use crate::file::Ready;
use alloc::collections::VecDeque;

/// Writes no larger than this are atomic, as on Linux.
pub const PIPE_BUF: usize = 4096;
pub const PIPE_CAPACITY: usize = 16 * 1024;

pub struct Pipe {
    bytes: VecDeque<u8>,
    reader: bool,
    writer: bool,
}

impl Default for Pipe {
    fn default() -> Self {
        Self::new()
    }
}

impl Pipe {
    pub fn new() -> Self {
        Self {
            bytes: VecDeque::with_capacity(PIPE_CAPACITY),
            reader: true,
            writer: true,
        }
    }

    /// Returns `EAGAIN` when a thread needs to wait. Buffered bytes precede EOF.
    pub fn read(&mut self, buf: &mut [u8]) -> KResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.bytes.is_empty() && self.writer {
            return Err(Errno::EAGAIN);
        }
        let count = buf.len().min(self.bytes.len());
        for byte in &mut buf[..count] {
            *byte = self.bytes.pop_front().unwrap();
        }
        Ok(count)
    }

    /// Small writes are all-or-nothing; large writes can return a partial count.
    pub fn write(&mut self, buf: &[u8]) -> KResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if !self.reader {
            return Err(Errno::EPIPE);
        }
        let room = PIPE_CAPACITY - self.bytes.len();
        if room == 0 || (buf.len() <= PIPE_BUF && room < buf.len()) {
            return Err(Errno::EAGAIN);
        }
        let count = room.min(buf.len());
        self.bytes.extend(&buf[..count]);
        Ok(count)
    }

    pub fn close_reader(&mut self) {
        self.reader = false;
    }

    pub fn close_writer(&mut self) {
        self.writer = false;
    }

    pub fn read_ready(&self) -> Ready {
        Ready {
            readable: !self.bytes.is_empty() || !self.writer,
            writable: false,
        }
    }

    pub fn write_ready(&self) -> Ready {
        Ready {
            readable: false,
            writable: !self.reader || PIPE_CAPACITY - self.bytes.len() >= PIPE_BUF,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn drain_before_eof_and_broken_pipe() {
        let mut pipe = Pipe::new();
        assert_eq!(pipe.read(&mut [0]), Err(Errno::EAGAIN));
        assert_eq!(pipe.write(b"hello"), Ok(5));
        pipe.close_writer();
        let mut buf = [0; 8];
        assert_eq!(pipe.read(&mut buf), Ok(5));
        assert_eq!(&buf[..5], b"hello");
        assert_eq!(pipe.read(&mut buf), Ok(0));
        assert!(pipe.read_ready().readable);
        pipe.close_reader();
        assert_eq!(pipe.write(b"x"), Err(Errno::EPIPE));
        assert_eq!(pipe.write(b""), Ok(0));
    }

    #[test]
    fn small_writes_are_atomic_and_large_writes_are_partial() {
        let mut pipe = Pipe::new();
        assert_eq!(
            pipe.write(&vec![1; PIPE_CAPACITY - 2]),
            Ok(PIPE_CAPACITY - 2)
        );
        assert_eq!(pipe.write(b"abc"), Err(Errno::EAGAIN));
        assert!(!pipe.write_ready().writable);
        assert_eq!(pipe.write(&vec![2; PIPE_BUF + 1]), Ok(2));
        assert_eq!(pipe.write(b"x"), Err(Errno::EAGAIN));
        let mut buf = vec![0; PIPE_CAPACITY];
        assert_eq!(pipe.read(&mut buf), Ok(PIPE_CAPACITY));
        assert!(buf[..PIPE_CAPACITY - 2].iter().all(|&b| b == 1));
        assert_eq!(&buf[PIPE_CAPACITY - 2..], &[2, 2]);
        assert!(pipe.write_ready().writable);
    }

    #[test]
    fn wraparound_preserves_byte_order() {
        let mut pipe = Pipe::new();
        for i in 0..100 {
            let data = vec![i; 701];
            assert_eq!(pipe.write(&data), Ok(data.len()));
            let mut result = vec![0; data.len()];
            assert_eq!(pipe.read(&mut result[..350]), Ok(350));
            assert_eq!(pipe.read(&mut result[350..]), Ok(351));
            assert_eq!(result, data);
        }
    }
}
