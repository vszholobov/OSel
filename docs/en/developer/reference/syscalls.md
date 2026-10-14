---
description: Linux x86_64 system-call numbers, arguments, OSel behavior and errors.
---

# System calls

**Audience:** developers writing OSel programs or adding a call.

**Summary:** use Linux x86_64 numbers/conventions: RAX number, RDI/RSI/RDX/R10/R8/R9 arguments, RAX result or negative errno. RCX/R11 are clobbered; other registers are preserved. Numbers use signed low 32 bits of RAX. Unknown calls return ENOSYS. [User mode](../architecture/core/user-mode.md#syscall) owns entry; docs-check compares both tables with `SYSCALLS` in `osel-core/src/syscall.rs`.

## Calls {#calls}

| Number | Call | Arguments | OSel behavior | Errors |
|---|---|---|---|---|
| 0 | `read` | fd, buf, count | At most 64 KB; regular-file shared offset; null returns 0; terminal/empty pipe wait; nonblocking EAGAIN; drained pipe without writers EOF | EBADF without read access; EFAULT before transfer; EISDIR; EINTR on kill |
| 1 | `write` | fd, buf, count | Terminal screen/serial, retaining split UTF-8; malformed bytes become replacement; null accepts all; pipe waits and may write partially | EBADF without write access; EFAULT with no bytes written; EAGAIN full nonblocking pipe; EPIPE no readers; EINTR |
| 3 | `close` | fd | Close descriptor; description lives through remaining references | EBADF |
| 5 | `fstat` | fd, statbuf | Linux x86_64 stat; see [files](../architecture/core/fs.md#open-files) | EBADF, EFAULT |
| 9 | `mmap` | addr, len, prot, flags, fd, offset | Anonymous only. MAP_SHARED behaves as MAP_PRIVATE without fork. Lazy pages; free rounded hint or highest range below MMAP_TOP; MAP_FIXED replaces, MAP_FIXED_NOREPLACE requires a free range | EINVAL zero length, unaligned offset, unsupported prot bits or absent PRIVATE/SHARED; EBADF or ENODEV for file mappings; ENOMEM range/space; EPERM fixed below 0x10000; EEXIST |
| 10 | `mprotect` | addr, len, prot | Change full-range rights; zero length no-op; incomplete coverage changes nothing | EINVAL alignment/prot; ENOMEM missing range |
| 11 | `munmap` | addr, len | Remove areas/free pages; empty mapped coverage is allowed | EINVAL alignment, zero length or range beyond lower half |
| 16 | `ioctl` | fd, request, arg | TCGETS, TCSETS, TCSETSW, TCSETSF and TIOCGWINSZ; Linux x86_64 structs | EBADF, ENOTTY, EFAULT; EINVAL nonzero VTIME/line discipline |
| 32 | `dup` | oldfd | Lowest free descriptor, clear FD_CLOEXEC | EBADF, EMFILE |
| 33 | `dup2` | oldfd, newfd | Replace exact descriptor and clear CLOEXEC; same number validates then no-op | EBADF |
| 35 | `nanosleep` | req, rem | Block for timespec at 10 ms granularity; kill writes remaining duration and returns EINTR | EINVAL negative/invalid tv_nsec; EFAULT; EINTR |
| 56 | `clone` | flags, child_stack, ptid, ctid, tls | Go thread flags VM/FS/FILES/SIGHAND/THREAD/SYSVSEM, optional CLONE_SETTLS; VM/VFORK/SIGCHLD creates a child with separate fd table and shared memory until exec/exit, blocking calling parent thread. Child gets 0, parent tid/PID | EINVAL other flags/stack; EFAULT TLS outside low half with SETTLS; ENOMEM; EAGAIN child limit |
| 59 | `execve` | path, argv, envp | Transactionally replace with static ELF/initial stack, close FD_CLOEXEC and reset FS/FPU; success does not return to old image; failure preserves image/files | ENOENT, EACCES, ENOEXEC, EFAULT, E2BIG, ENOMEM; EINVAL multithreaded process |
| 61 | `wait4` | pid, status, options, rusage | Wait for positive PID or any child (-1), consume result once, return Linux status; WNOHANG returns 0 for a live child; rusage zero | ECHILD; EINVAL selectors/flags; EFAULT without losing result; EINTR |
| 110 | `getppid` | — | Parent PID; kernel-launched root returns 0 | — |
| 24 | `sched_yield` | — | Yield to next ready thread | — |
| 28 | `madvise` | addr, len, advice | DONTNEED/FREE release pages for zero-filled next access; other supported Linux advice no-op; zero length no-op | EINVAL alignment/unknown advice/MADV_REMOVE; ENOMEM uncovered range, after handling covered parts |
| 202 | `futex` | uaddr, op, val, timeout | WAIT on equal word with relative monotonic timeout; WAKE up to val and return count. Only these operations, with/without FUTEX_PRIVATE_FLAG | EAGAIN changed word; ETIMEDOUT; EINTR; EFAULT; ENOSYS other operations |
| 233 | `epoll_ctl` | epfd, op, fd, event | ADD/MOD/DEL watched descriptor/events/data | EPERM nonpollable regular file/terminal (Go assumes always ready); EEXIST, ENOENT, EINVAL, EBADF, EFAULT |
| 281 | `epoll_pwait` | epfd, events, maxevents, timeout | Wait for readiness; timeout in ms, negative infinite, zero immediate; eventfd writes wake | EINVAL maxevents zero; EINTR; EBADF; EFAULT |
| 290 | `eventfd2` | initval, flags | Counter fd; 8-byte write adds, read consumes all or one with EFD_SEMAPHORE; empty EFD_NONBLOCK read EAGAIN; epollable | EINVAL flags; EMFILE |
| 291 | `epoll_create1` | flags | Create epoll, only EPOLL_CLOEXEC | EINVAL, EMFILE |
| 292 | `dup3` | oldfd, newfd, flags | Exact duplicate with different numbers; only O_CLOEXEC | EBADF, EINVAL |
| 293 | `pipe2` | pipefd, flags | Create read/write descriptors and write two ints; O_CLOEXEC/O_NONBLOCK; bad pointer or fd exhaustion leaks nothing; see [pipes](../architecture/core/fs.md#pipes) | EINVAL, EFAULT, EMFILE |
| 228 | `clock_gettime` | clock_id, timespec | MONOTONIC from boot, REALTIME from Unix epoch via CMOS; COARSE/RAW/BOOTTIME variants use same clocks | EINVAL unknown clock; EFAULT |
| 186 | `gettid` | — | Current thread ID | — |
| 39 | `getpid` | — | Process PID shared by threads; monotonic unique root/child sequence | — |
| 60 | `exit` | status | Exit current thread; last thread ends process; low 8 status bits | — |
| 72 | `fcntl` | fd, cmd, arg | F_DUPFD, F_DUPFD_CLOEXEC, F_GETFD/F_SETFD, F_GETFL (with O_LARGEFILE), F_SETFL (only APPEND/NONBLOCK) | EBADF first; EINVAL command/bad lower bound; EMFILE |
| 231 | `exit_group` | status | Exit whole process, cancel threads/waits; low 8 status bits | — |
| 257 | `openat` | dirfd, path, flags, mode | Open file/directory/device; relative to dirfd or root with AT_FDCWD; Linux flag error order | ENOENT, ENOTDIR, EISDIR, EROFS, EEXIST, EMFILE, EBADF dirfd, EFAULT, ENAMETOOLONG |
| 262 | `newfstatat` | dirfd, path, statbuf, flags | Path stat; AT_EMPTY_PATH/empty path stats dirfd; no symlinks, AT_SYMLINK_NOFOLLOW no-op | Same as openat/fstat |
| 13 | `rt_sigaction` | sig, … | Stub returns 0 without retaining handler/delivering signals | EINVAL signals 32,33,64 |
| 14 | `rt_sigprocmask` | how, set, oldset | Empty mask; write zeros to oldset and return 0 | EFAULT |
| 131 | `sigaltstack` | ss, old_ss | No alternate stack; old_ss gets SS_DISABLE | EFAULT |
| 157 | `prctl` | — | PR_SET_VMA and others return EINVAL, ignored by Go | EINVAL |
| 158 | `arch_prctl` | code, addr | ARCH_SET_FS below USER_END; ARCH_GET_FS reads base | EINVAL other codes; EFAULT noncanonical address |
| 204 | `sched_getaffinity` | pid, cpusetsize, mask | Single CPU, bit 0 mask, return 8; Go sets GOMAXPROCS=1 | EINVAL size below 8; EFAULT |
| 234 | `tgkill` | — | No-op; no signals, async Go preemption disabled through GODEBUG | — |
| 302 | `prlimit64` | pid, resource, new, old | RLIMIT_NOFILE 1024, RLIMIT_STACK 8 MB, others unlimited; ignore new | EFAULT |
| 318 | `getrandom` | buf, len, flags | Fill with noncryptographic pseudorandom bytes | EFAULT |

fd/dirfd and madvise advice use signed low 32 bits like Linux int. prot bits above 32 return EINVAL.

Add a call number/name to SYSCALLS, a dispatch branch in `src/syscall.rs`, and a row in both tables. Access program addresses only through copy_from_user/copy_to_user; see [validated memory](../architecture/core/user-mode.md#user-mem).

waitid, pidfd_open, clone3 and full fork return ENOSYS, allowing Go's vfork-style clone/wait4 fallback. SIGCHLD flags provide ABI compatibility without signal delivery; EPIPE does not send SIGPIPE.

execve permits at most 1023 argv and 1023 envp strings, together at most 128 KiB. The completed pointer/auxv stack must also fit 128 KiB. Exceeding either bound gives E2BIG before replacing the old image.

_Checked against code: 2026-10-13, commit 6f96d29 and stage-2.5 work._
