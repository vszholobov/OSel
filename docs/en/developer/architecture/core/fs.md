---
description: cpio ramdisk, storage/node interfaces, open descriptions, devices, terminal, pipes and Linux path errors.
---

# Files and ramdisk

**Audience:** developers reading files, adding image contents or connecting storage.

**Summary:** runner archives ramdisk/ and built /bin programs in cpio. Bootloader loads it beside the kernel; osel::init mounts it as /. Kernel file consumers use BlockDevice/FileSystem/Inode from osel-core. Ramdisk is one implementation and errors are Linux-numbered Errno.

## Behavior {#how}

### Repository file to kernel {#path}

pack_ramdisk in `runner/src/main.rs` walks ramdisk/ or OSEL_RAMDISK in name order, adding regular files/directories with zero owners/times through CpioBuilder for deterministic bytes, then built user programs. DiskImageBuilder::set_file_contents places ramdisk in the FAT partition without a temporary archive; both firmware modes receive the same contents.

BootInfo supplies ramdisk_addr/ramdisk_len, mapped high and reserved from the frame pool. `src/fs.rs` wraps it in MemBlockDevice, mounts CpioFs and stores ROOT_FS. lookup/read_file serve consumers; normal boot prints /etc/motd.

### Layers {#layers}

| Interface | Location | Current implementation and future extension |
|---|---|---|
| BlockDevice | `osel-core/src/block.rs` | 512-byte MemBlockDevice; future AHCI/NVMe/virtio |
| FileSystem | `osel-core/src/vfs.rs` | CpioFs; future ext2/FAT |
| Inode | `osel-core/src/vfs.rs` | stat/read_at/lookup/parent/read_dir; future node types |
| resolve | `osel-core/src/vfs.rs` | Root/cwd name traversal; future mounts/symlinks |
| Errno/KResult | `osel-core/src/errno.rs` | Linux errors |

Default Inode methods return the appropriate error: file lookup/parent ENOTDIR, directory read_at EISDIR. read_dir starts at opaque cursor zero and returns an entry/next cursor (cpio uses an index; disks can use an offset). Stat::default and non_exhaustive FileType permit extension without rewriting implementations.

### Devices {#devices}

WithDevices in `osel-core/src/devices.rs` overlays /dev above any ramdisk dev directory. DEVICES contains null (1:3) and tty (5:0), ordinary CharDevice nodes with rdev. Normal resolve supports equivalent spellings such as //dev/null and /etc/../dev/tty. See [0014](../../decisions/0014-file-descriptors.md).

### Open files {#open-files}

`osel-core/src/file.rs` separates:

- FileLike: read/write/stat/inode, seekable, poll and epollable; unsupported operations error. InodeFile wraps filesystem nodes, NullFile serves /dev/null, and Tty in `src/tty.rs` serves the terminal. fs::file_for selects by node/device type.
- OpenDescription: shared file, access mode, O_APPEND/O_NONBLOCK and offset. dup shares it. Wrong access returns EBADF. Advance offset only for transferred bytes; nonseekable devices skip OFFSET so they can block.
- FdTable: lowest free descriptor below MAX_FDS 1024, otherwise EMFILE; dup with lower bound, dup_to exact replacement, per-descriptor FD_CLOEXEC. Child table copies share descriptions; successful exec closes marked entries, failed exec preserves them. get clones Arc before releasing table ownership.

open_access follows Linux read-only error order: missing O_CREAT/O_TMPFILE gives EROFS; existing O_CREAT|O_EXCL gives EEXIST; writing/creating a directory gives EISDIR; O_DIRECTORY on a non-directory gives ENOTDIR; ordinary-file write/truncate/access-mode-3 gives EROFS; creation with trailing slash gives EISDIR. Devices accept write and ignore truncate. O_PATH returns EINVAL.

encode_stat writes 144-byte Linux x86_64 stat: device 1, link count (2 for directories), mode/rdev/size, block size 4096 and 512-byte block count; owner/times zero.

Tty merges PS2/COM1 input, sends output to screen/serial and retains split UTF-8 characters between writes. Empty reads block (or EAGAIN with O_NONBLOCK), with atomic masked check/enqueue/block. tty::forget removes dead waiters. OutputGuard owns serialization under brief TTY_OUTPUT; competitors sleep without spinlocks. Decode 64 input bytes at a time into bounded stack storage, lock framebuffer per character and COM1 per nonblocking attempt. Cancellation/error releases output and wakes writers. Read-side echo preserves incoming IF.

`osel-core/src/terminal.rs` stores 36-byte Linux termios and bounded input. Default is immediate input without echo. TCGETS/TCSETS/TCSETSW/TCSETSF support ICANON/ECHO, CRLF conversion and VMIN. Canonical mode handles Backspace/EOF; TCSETSF flushes input. TCSETSW does not wait because output is synchronous. Echo runs in the reading thread, not IRQ. VTIME nonzero and nonzero line discipline return EINVAL; signals/full termios are unsupported. TIOCGWINSZ uses framebuffer geometry or 80×25. All terminal opens share modes/input/UTF-8 state.

Programs start with terminal descriptors 0–2 through user::standard_files. MemBlockDevice wraps static bytes or heap Vec/Arc; read_bytes handles arbitrary offsets over blocks.

### epoll and eventfd {#epoll}

`osel-core/src/poll.rs` provides FileLike EventFd and Epoll. Eight-byte eventfd writes add to a counter; reads consume all or one with EFD_SEMAPHORE. Empty nonblocking reads give EAGAIN; nonzero counters are readable/epollable. epoll_ctl adds/modifies/removes fd/events/data; regular files/terminal are not epollable and return EPERM, which Go uses to treat them as always ready.

epoll_pwait in `src/poll.rs` polls watched objects, then blocks until timeout via SLEEPERS or notification. Eventfd writes wake waiters to recheck. Pipes also report readiness and notify on data/end changes; empty pipes without writers are readable because read will return EOF.

### Pipes {#pipes}

pipe2 creates read-only/write-only descriptions over a 16 KiB buffer. Writes through PIPE_BUF 4096 are atomic and wait for sufficient space; longer writes can be partial. O_NONBLOCK/O_CLOEXEC can be set at creation; fcntl status is shared by description copies.

Empty pipes with writers block readers; full pipes block writers; nonblocking calls return EAGAIN. Check/register/block with interrupts masked, releasing guards before switching. Read frees space, write adds bytes, last endpoint close wakes waiters. dup/inheritance keep endpoints alive until the last description reference. After writer close, drain bytes then EOF; after reader close, write gives EPIPE without SIGPIPE. Pure buffer logic is `osel-core/src/pipe.rs`; kernel waits/endpoints are `src/pipe.rs`.

### cpio format {#cpio}

newc magic 070701 is used; 070702 is accepted without checksum validation. CpioFs::mount builds a heap tree once, sorts children and leaves file contents on the device for read-at access. Root inode is 1. Hard-linked regular files with nlink>1 and identical ino/devmajor/devminor share a node; data may occur in any link entry, including the last as GNU/libarchive emit.

Only files/directories are accepted. EINVAL rejects symlinks/devices/other types; malformed hex/name length/termination/UTF-8; embedded NUL or .. paths; components over 255 bytes; regular files ending / or /.; duplicate/conflicting paths/root entries; file root; directory data; out-of-device data or missing TRAILER!!!. Missing parent directories are synthesized with 0755. ., ./ and / describe the root.

### Path resolution {#resolve}

resolve(root,cwd,path) starts at root for absolute paths and cwd otherwise; resolve_absolute starts at root. Skip repeated / and ., traverse .. through parent without leaving root. Root identity uses inode number, so cwd must share the filesystem until mounts exist.

| Condition | Error |
|---|---|
| Empty path or absent name | ENOENT |
| File in an intermediate component, even before an overlong later name | ENOTDIR |
| Trailing slash, . or .. after a file | ENOTDIR |
| Component > NAME_MAX 255 or path ≥ PATH_MAX 4096 | ENAMETOOLONG |

## Code {#code}

`osel-core/src/errno.rs`, `osel-core/src/block.rs`, `osel-core/src/vfs.rs`, `osel-core/src/cpio.rs`, `osel-core/src/devices.rs`, `osel-core/src/file.rs`: interfaces/archive/nodes/descriptors. `src/fs.rs`: init/ramdisk/root/lookup/file_for/read_file. `src/tty.rs`: Tty. `osel-core/src/pipe.rs`/`src/pipe.rs`: pipe buffers/waits. `src/syscall.rs`: file calls. `runner/src/main.rs`: ramdisk_dir/build_user_programs/pack_ramdisk/create_disk_image. `ramdisk/`: shared boot/test contents.

## Invariants {#invariants}

Ramdisk is read-only (write_blocks EROFS), remains alive and reserved for the kernel lifetime. Root is immutable after mount; Arc retains prior nodes. Missing ramdisk permits boot with root None/lookup ENOENT; corrupt ramdisk panics because the image is invalid. osel-core does not know where boot bytes originate.

## Locking {#locks}

ROOT_FS is held only to clone Arc. Plain OFFSET guards one seekable read/write. Tty guards text/input/serial/wait queues but releases before sleeping. Mounted trees do not change. IRQ handlers do not mount/read files because those operations may allocate.

## Pitfalls {#pitfalls}

A zero-length ramdisk file breaks UEFI bootloader allocation; even an empty directory must yield TRAILER!!! padded to 512 bytes. cpio data is only four-byte aligned, so copy ELF segments. Read large files incrementally; read_to_end cannot exceed the 16 MB heap and returns ENOMEM. Linux fd/dirfd are signed low 32 bits; Go AT_FDCWD may appear as 0xFFFFFF9C, so use fd_arg. Read paths to NUL page-by-page rather than prevalidating all 4096 bytes. lookup creates new objects; compare inode numbers, not pointers. BIOS reads large ramdisks slowly through firmware. Git stores only execute permission, so checkout umask changes other modes. Runner rejects symlinks and skips .DS_Store.

## Tests {#tests}

Host tests cover block boundaries/partial blocks/large blocks/heap storage, path rules/error order/limits/default methods, cpio builder and real hardlink fixtures, both link orders, corruption/truncation at every boundary, 20000 random corruptions and 300 random-tree round trips. Devices test path equivalence/overlay/numbers. Descriptors test open precedence/shared offsets/inheritance/independent closes/exact dup/lowest free/limits/flags/model/stat. Pipes test atomic/partial/wrapping/EOF/EPIPE.

Kernel tests cover blocking/nonblocking pipe wakes and final-copy endpoint lifetimes. `tests/go_exec.rs` checks clear, argv/envp, code 37, missing binaries, repeats and large transfers. `tests/child_processes.rs` checks bad pipe2 pointers without leaks, dup2/3, atomic nonblocking writes, transactional exec/CLOEXEC, WNOHANG/bad status/single consumption, parent teardown with blocked descendants, kill of futex/sleep, and child-limit recovery after wait. File syscall tests cover motd/stat/open errors/boundary paths/high fd bits/read/double-close/fcntl/input/automatic cleanup. Runner tests check packaging/collisions/setuid/.DS_Store/determinism/empty/missing/symlink inputs. `tests/ramdisk.rs` checks both firmware paths, motd/all files/relative paths and reserved ramdisk frames.

## Decisions {#decisions}

[0007](../../decisions/0007-storage-interfaces.md), [0014](../../decisions/0014-file-descriptors.md), [0005](../../decisions/0005-linux-abi-for-go.md).

_Checked against code: 2026-10-13, commit 6f96d29 and stage-2.5 work._
