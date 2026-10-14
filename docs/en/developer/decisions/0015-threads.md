---
description: Why threads own kernel stacks, switch in the timer handler and eagerly preserve FPU/FS state.
---

# 0015. Threads and preemption

**Audience:** developers changing scheduling, entry or timers.

**Summary:** each thread owns a kernel stack with TrapFrame at its top. Timer switches directly in x86-interrupt without a full user frame. switch_to saves callee-saved/RFLAGS; Rust changes CR3/RSP0/current stack/FS/FPU around it. This replaces [0011](0011-user-entry.md)'s run-until-fault, [0012](0012-syscall-entry-and-user-programs.md)'s launch stack and [0013](0013-fpu-state.md)'s launch-bound FPU save. See [threads](../architecture/core/threads.md).

**Status:** partially superseded by [0017](0017-child-processes.md) for termination and address-space ownership. **Date:** 2026-10-13.

## Context

Step 8 needs Go OS threads, futex/nanosleep waits and preemption on one CPU. Previously programs ran on the launcher's stack until exit.

## Decision

Give each thread a guarded 64 KB stack in a separate level-4 entry; blocked calls retain it. Place the 16-byte-aligned frame directly below the aligned top for exception/syscall/first entry. After EOI, the compiler timer handler calls schedule and keeps its saved registers on the outgoing stack. The anticipated timer stub is unnecessary: full frames serve calls/exceptions, not switching.

switch_to saves only callee-saved/RFLAGS and returns the previous thread; Rust changes other context. Select under SCHEDULER, unlock before switch, keep interrupts masked throughout. The next thread immediately frees a dead predecessor, without an idle-thread zombie list. Schedule round-robin every tick via replaceable SchedPolicy. kill is checked before user return.

## Alternatives considered

One stack per CPU cannot retain blocked kernel waits. A full timer frame is deferred until signal delivery needs it. Lazy FPU switching complicates preemption. Passing IrqSpinlock across switch_to prevents the next thread from correctly releasing its caller/IF metadata. Idle-only zombie cleanup delays reclamation unpredictably and breaks leak tests.

## Consequences

Interrupt-enabled syscalls are preemptible; IrqSpinlock regions are not. At this step one-thread processes need no space/file locks; clone adds them later. Timer-preempted threads lack a complete frame, so a timer-killed process returns an empty frame. Mark 0011/0012/0013 partially superseded.
