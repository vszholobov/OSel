---
description: Initial syscall stack, checked sysret/iret return paths and runner-built assembly programs.
---

# 0012. syscall entry and user/ programs

Language: [Русский](../../../ru/developer/decisions/0012-syscall-entry-and-user-programs.md) · [English](0012-syscall-entry-and-user-programs.md)

**Audience:** developers changing calls or ramdisk executables.

**Summary:** syscall builds the exception TrapFrame on the launching kernel stack, enables interrupts and dispatches in Rust. Return uses sysretq, iretq for TF or program termination for an invalid address. Runner builds user/ assembly source into /bin. See [user mode](../architecture/core/user-mode.md#syscall) and [calls](../reference/syscalls.md).

**Status:** partially superseded by [0015](0015-threads.md). **Date:** 2026-10-12.

## Context

Stage-2 step 5 prints/exits with Linux calls. No threads exist yet; [0011](0011-user-entry.md) runs until exit. The check needs a program from ramdisk, while [0007](0007-storage-interfaces.md) excludes binary files from Git.

## Decision

Use KERNEL_RSP, the launch stack also used for exceptions, until per-thread stacks arrive. SFMASK clears IF/DF/TF/AC/NT/IOPL; enable interrupts only after switching stacks, checked in entry machine code. Reuse TrapFrame with vector 256; exit leaves launch as exceptions do, with status in RDI.

return_path in osel-core selects sysretq, iretq with TF (avoiding a one-instruction-early trap), or #GP termination for an out-of-low-half address that would otherwise fault in ring 0. Ignore the high RAX half for Linux's 32-bit call number.

Store assembly source in independent no_std user/ crates with global_asm! and _start, no private support library. Runner builds static x86_64-unknown-none ET_EXEC in user/target and adds /bin entries.

## Alternatives considered

Separate syscall stacks/swapgs were deferred to threads. Always using iretq is slower for thousands of Go calls. Ready-made ramdisk ELF puts binaries in Git. Nested Cargo from kernel build.rs hits the target lock and leaves runner without OUT_DIR. Raw Rust instruction-byte arrays are unreadable and fragile. Runner already owns archive construction, so it owns program builds.

## Consequences

Runner invokes Cargo every time, cheaply when unchanged. A conflicting bin/<name> file fails packaging. All normal/test kernels receive the same programs.
