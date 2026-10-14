---
description: Why initial user launch ran until an exception on the caller stack, without swapgs, and used ring-selecting exception stubs.
---

# 0011. Run until exception and entry stubs

**Audience:** developers changing program entry, exceptions or calls.

**Summary:** initially user::run used iretq from the caller's kernel stack and returned on a user exception. A static pointer and TSS RSP0 selected that stack without swapgs. Stubs sent ring-0 exceptions unchanged to existing handlers and built a complete user TrapFrame. See [current user mode](../architecture/core/user-mode.md).

**Status:** partially superseded by [0015](0015-threads.md). **Date:** 2026-10-12.

## Context

Stage-2 step 4 needed safe ring-3 execution, containment of every user exception and all program registers. There was one CPU and no threads/scheduler until step 8. Kernel x86-interrupt handlers already supported the crash tests.

## Decision

Run saves callee-saved registers/flags on the caller stack; user_trap jumps back to it on a fault. This suffices for one program before calls; step 8 replaces the return with switching. No swapgs/GS: the stack pointer is static and in RSP0, and compiler handlers cannot perform conditional swapgs.

Ring-selecting stubs preserve ring-0 stacks/behavior and construct a full ring-3 TrapFrame. Hardware IRQs retain compiler handlers, which save the registers they clobber. This decision anticipated a timer stub for preemption; [0015](0015-threads.md) later avoided it. IST double fault/NMI/machine check have no stubs and still panic from ring 3.

## Alternatives considered

Checking the ring inside x86-interrupt handlers cannot recover every user register: the compiler saves only clobbered registers at unspecified locations. Replacing every vector with a custom assembly/Rust entry would rewrite kernel exception handling and exact-address tests. A per-program RSP0 stack with per-CPU GS data was deferred until threads/multiple CPUs.

## Consequences

At this step the program cannot wait or be preempted and runs until an exception. Step 5 adds syscall with the same frame. Run requires interrupts enabled and no held locks. Every new user-capable exception needs entry_stub or it can panic the kernel.
