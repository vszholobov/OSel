---
description: Why OSel loads static ELF64 through a small parser and ProgramMemory abstraction.
---

# 0010. Static ELF loader

**Audience:** developers changing program loading or initial memory.

**Summary:** OSel accepts static x86_64 ELF64 executables (`ET_EXEC`). A small `osel-core` parser uses `ProgramMemory` to map zeroed pages and copy bytes, making loading host-testable. The main stack is mapped eagerly.

**Status:** accepted. **Date:** 2026-10-12.

## Context

[Stage 2](0005-linux-abi-for-go.md) requires a static Go binary. With `CGO_ENABLED=0`, Go produces `ET_EXEC` with three `PT_LOAD` segments and no interpreter. ELF is untrusted input; malformed-file testing in QEMU would be slow.

## Decision

Parse only supported fields and reject errors with `ENOEXEC`. Accept only `ET_EXEC`; reject `PT_INTERP` and `PT_DYNAMIC`. Dynamic linking and PIE are unnecessary for Go and assembly tests. `ProgramMemory` separates parsing/loading from page tables and enables byte-accurate host tests and fuzzing. Reject writable executable segments or shared pages. Map the 256 KB stack immediately; demand paging is used for `mmap` later.

## Alternatives considered

`xmas-elf` or `goblin` parse more than needed but do not replace range checks. Loading directly into `UserSpace` would require QEMU tests. Mapping ramdisk bytes directly fails because cpio contents are not page-aligned. PIE (`ET_DYN`) needs relocation and `AT_BASE` and is deferred.

## Consequences

Dynamic binaries and C programs built with `-static-pie` fail with `ENOEXEC`. Each program reserves 256 KB of physical stack memory. See [programs](../architecture/core/programs.md).
