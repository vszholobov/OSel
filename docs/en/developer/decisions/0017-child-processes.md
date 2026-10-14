---
description: Why Go os/exec uses a checked vfork-style clone, transactional execve and cooperative process teardown.
---

# 0017. Child processes

**Audience:** developers changing process creation, exec, waits or cancellation.

**Summary:** child processes have separate descriptor tables, share memory until `execve` or exit, and retain results until `wait4`. Process termination interrupts blocked calls so Rust resources are released before thread stacks disappear.

**Status:** accepted. **Date:** 2026-10-13. Partially supersedes [0015](0015-threads.md) for process termination and address-space ownership; context switching is retained.

## Context

Tetris runs an external `clear` through Go `os/exec`. Thread-only clone cannot provide child PID, execution replacement, inherited descriptors, launch-error pipes or wait results.

## Decision

- Keep distinct processes with PID and parent/child bookkeeping while preserving Go's thread-clone path.
- Support `clone(CLONE_VM | CLONE_VFORK | SIGCHLD)`. The child temporarily shares `Arc<IrqSpinlock<UserSpace>>`; only the calling parent thread waits. Full `fork` is unsupported.
- Build an `execve` image before committing the replacement. On success switch CR3, close `CLOEXEC` and wake the parent; on failure keep the old image. Replacement requires a single-threaded process, one CPU, interrupts disabled and no guard holding the old `MM`.
- `kill` and `exit_group` wake blocked calls to return `EINTR`, release locals, and terminate before user return. A later thread frees the old stack after switching away.
- Parent termination kills and waits for descendants; there is no init adopter.
- Close child descriptors on exit so pipe EOF does not depend on `wait4`. Keep the terminated child and its address space until wait consumes it. Limit each parent to 128 unreaped children; compact zombies and early `MM` release are deferred.

## Alternatives considered

Full fork/copy-on-write adds page sharing and fault logic unnecessary for Go's current path. Pretending unsupported Go probes succeed selects mechanisms OSel lacks: pidfd and `waitid` return `ENOSYS`, and QEMU checks the fallback. Immediately deleting blocked stacks leaks local resources. A separate init and compact zombie protocol would suit a general OS but require additional ownership rules.

## Consequences

External `/bin/clear`, launch errors, argv/envp, nonzero exit codes and pipes work. Every new blocking call must support cancellation or process-tree teardown can hang. The child-count bound is not a memory bound. Multi-CPU and multithreaded exec require a new replacement protocol.

Go 1.27.1's path was checked against the local toolchain and `tests/go_exec.rs`; Linux `strace` was not collected. Recheck fallbacks after toolchain updates.
