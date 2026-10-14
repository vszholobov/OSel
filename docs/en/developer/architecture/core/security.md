---
description: Ring-3 memory and system-call isolation guarantees and adversarial tests.
---

# Program isolation

**Audience:** developers changing user mode, calls or program memory.

**Summary:** ring-3 programs cannot access kernel/other-process memory, redirect kernel execution through pointers or crash it by spawning threads. Protection uses page tables and pointer checks rather than SMAP/SMEP. `src/security.rs` checks failures remain contained and leak-free.

## Guarantees {#guarantees}

Upper-half pages lack USER_ACCESSIBLE; user reads/writes/jumps fault without panicking the kernel. Each process owns its lower half. Syscall addresses are checked through `user_mem::check` for range, presence and user rights, then accessed through the physical mapping; invalid pointers return EFAULT without data transfer. Mapping calls cannot affect the upper half, overflow addr+len, or map below MIN_USER_ADDR; errors are ENOMEM/EINVAL/EPERM as appropriate.

Equal virtual addresses in separate processes refer to distinct zeroed physical pages. User frames are sanitized for ring 3; noncanonical/kernel return addresses terminate with #GP. `clone` is bounded by MAX_THREADS/kernel stack slots, returning ENOMEM at exhaustion. `exit_group`, faults and kill collect threads and release stacks.

## Limits {#limits}

Signals are not delivered, so Go nil access terminates instead of becoming a Go panic. SMAP/SMEP are not enabled. General resource limits are absent beyond descriptor and stack counts; programs can consume available scheduled CPU and memory.

## Tests {#tests}

`src/security.rs` tests kernel-address reads/writes/jumps, bad pointers to write/futex, mapping boundaries/overflow, EBADF, clone flooding and same-address processes. Every test calls `kernel_is_fine` and checks frame/heap recovery. Other hostile inputs are covered by [user mode](user-mode.md) and `src/syscall.rs`.

## Related pages {#links}

[Memory](memory.md), [user mode](user-mode.md) and [threads](threads.md).

_Checked against code: 2026-10-13, commit stage-2-time._
