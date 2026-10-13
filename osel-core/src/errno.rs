//! Linux error codes, the one error type of every kernel interface that user
//! programs can reach: system calls return them as `-errno`

use core::fmt;

/// An error code with its number from Linux `asm-generic/errno-base.h` and `errno.h`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum Errno {
    /// Operation not permitted
    EPERM = 1,
    /// No such file or directory
    ENOENT = 2,
    /// Interrupted: the call stopped waiting
    EINTR = 4,
    /// Input/output error
    EIO = 5,
    /// No such device or address
    ENXIO = 6,
    /// Argument list too long
    E2BIG = 7,
    /// Exec format error
    ENOEXEC = 8,
    /// Bad file descriptor
    EBADF = 9,
    /// Try again: nothing to read without waiting
    EAGAIN = 11,
    /// Out of memory
    ENOMEM = 12,
    /// Bad address
    EFAULT = 14,
    /// File exists
    EEXIST = 17,
    /// No such device: the file can't be mapped
    ENODEV = 19,
    /// Not a directory
    ENOTDIR = 20,
    /// Is a directory
    EISDIR = 21,
    /// Invalid argument
    EINVAL = 22,
    /// Too many open files
    EMFILE = 24,
    /// Not a terminal
    ENOTTY = 25,
    /// No space left on device
    ENOSPC = 28,
    /// Read-only file system
    EROFS = 30,
    /// File name too long
    ENAMETOOLONG = 36,
    /// Function not implemented
    ENOSYS = 38,
    /// Connection timed out: a `futex` or wait deadline passed
    ETIMEDOUT = 110,
}

impl Errno {
    /// The number Linux uses for this error
    pub const fn code(self) -> i32 {
        self as i32
    }

    /// The symbolic name, as in the C headers
    pub const fn name(self) -> &'static str {
        match self {
            Errno::EPERM => "EPERM",
            Errno::ENOENT => "ENOENT",
            Errno::EINTR => "EINTR",
            Errno::EIO => "EIO",
            Errno::ENXIO => "ENXIO",
            Errno::E2BIG => "E2BIG",
            Errno::ENOEXEC => "ENOEXEC",
            Errno::EBADF => "EBADF",
            Errno::EAGAIN => "EAGAIN",
            Errno::ENOMEM => "ENOMEM",
            Errno::EFAULT => "EFAULT",
            Errno::EEXIST => "EEXIST",
            Errno::ENODEV => "ENODEV",
            Errno::ENOTDIR => "ENOTDIR",
            Errno::EISDIR => "EISDIR",
            Errno::EINVAL => "EINVAL",
            Errno::EMFILE => "EMFILE",
            Errno::ENOTTY => "ENOTTY",
            Errno::ENOSPC => "ENOSPC",
            Errno::EROFS => "EROFS",
            Errno::ENAMETOOLONG => "ENAMETOOLONG",
            Errno::ENOSYS => "ENOSYS",
            Errno::ETIMEDOUT => "ETIMEDOUT",
        }
    }
}

impl fmt::Display for Errno {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Result of a kernel operation that can fail with a Linux error
pub type KResult<T> = Result<T, Errno>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_match_linux() {
        // asm-generic/errno-base.h and errno.h, the same on x86_64
        let expected = [
            (Errno::EPERM, 1),
            (Errno::ENOENT, 2),
            (Errno::EINTR, 4),
            (Errno::EIO, 5),
            (Errno::ENXIO, 6),
            (Errno::E2BIG, 7),
            (Errno::ENOEXEC, 8),
            (Errno::EBADF, 9),
            (Errno::EAGAIN, 11),
            (Errno::ENOMEM, 12),
            (Errno::EFAULT, 14),
            (Errno::EEXIST, 17),
            (Errno::ENODEV, 19),
            (Errno::ENOTDIR, 20),
            (Errno::EISDIR, 21),
            (Errno::EINVAL, 22),
            (Errno::EMFILE, 24),
            (Errno::ENOTTY, 25),
            (Errno::ENOSPC, 28),
            (Errno::EROFS, 30),
            (Errno::ENAMETOOLONG, 36),
            (Errno::ENOSYS, 38),
            (Errno::ETIMEDOUT, 110),
        ];
        for (errno, code) in expected {
            assert_eq!(errno.code(), code, "{errno}");
        }
    }

    #[test]
    fn display_is_the_symbolic_name() {
        assert_eq!(std::format!("{}", Errno::ENOENT), "ENOENT");
        assert_eq!(Errno::ENAMETOOLONG.name(), "ENAMETOOLONG");
    }
}
