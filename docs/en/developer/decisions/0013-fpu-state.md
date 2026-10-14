---
description: Why user x87/SSE was initially saved around execution with fxsave64, while XSAVE/AVX remain disabled.
---

# 0013. Program x87 and SSE state

**Audience:** developers changing program launch, switching or CR0/CR4.

**Summary:** the soft-float kernel does not touch x87/SSE. Load a complete 512-byte fxsave64 state before execution and save it afterward, without lazy CR0.TS switching. Disable XSAVE/AVX. See [current state switching](../architecture/core/user-mode.md#fpu).

**Status:** partially superseded by [0015](0015-threads.md). **Date:** 2026-10-12.

## Context

Go GOAMD64=v1 uses SSE2 for memory copies, hashes and floating point. BIOS does not enable SSE and instructions fault with #UD; UEFI often does. Even sequential programs must not see one another's registers.

## Decision

fpu::init configures CR0/CR4 explicitly; x87 faults use #MF rather than masked legacy IRQ13. Run loads/saves full state around execution, while x86_64-unknown-none kernel and interrupt handlers do not alter it. Fresh state has FCW 0x037F, MXCSR 0x1F80, empty x87 and zero XMM. Clear CR4.OSXSAVE; Go only checks XCR0 with it set and can use SSE2 without AVX.

## Alternatives considered

Lazy CR0.TS/#NM saves work for unused state but complicates preemption. XSAVE/AVX requires variable-size storage, CPUID and XCR0 and is unnecessary for GOAMD64=v1. Kernel SSE would require preserving user state on every interrupt, without a kernel need for floating point.

## Consequences

Step 8 moves state into each thread and saving into switching: a preempted thread does not return through run.
