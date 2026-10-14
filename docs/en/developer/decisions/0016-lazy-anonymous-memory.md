---
description: Why mmap uses permission areas and demand-allocated anonymous pages, retaining data through PROT_NONE.
---

# 0016. Anonymous memory on first access

Language: [Русский](../../../ru/developer/decisions/0016-lazy-anonymous-memory.md) · [English](0016-lazy-anonymous-memory.md)

**Audience:** developers changing mmap, user faults or low-half tables.

**Summary:** VmaSet describes areas/rights and pages appear on first access. Anonymous mappings only; leaf entries own rights while intermediate tables permit all access. PROT_NONE clears PRESENT but retains the frame. See [memory](../architecture/core/memory.md#areas).

**Status:** accepted. **Date:** 2026-10-13.

## Context

Go reserves about 1.1 GB PROT_NONE and 40 MB read/write, touching little. It uses MAP_FIXED inside reservations, releases with MADV_DONTNEED expecting zeros, and treats munmap failures as fatal. Test QEMU has 128 MB.

## Decision

Mappings in osel-core keeps areas outside page tables and uses PageTables for host/model-tested semantics; kernel takes MEMORY per table operation. mmap reserves, a permitted missing-page fault allocates zeroed memory and resumes the instruction, and kernel copies fault pages in the same way.

File mapping returns ENODEV; anonymous MAP_SHARED behaves as PRIVATE without fork. Intermediate tables are PRESENT|WRITABLE|USER_ACCESSIBLE. PROT_NONE retains the address without PRESENT; traversals check entry emptiness so release remains possible. Choose free hints or top-down below MMAP_TOP, one GB below USER_END. Go hints from 0xc000000000 in 2^40 steps through 0x7fc000000000 and Go 1.26 random bases below 2^46 fit. ELF/stack are eagerly mapped areas, allowing ordinary unmap/protect/fixed replacement.

## Alternatives considered

Eagerly mapping Go reserves needs over a GB. Removing USER_ACCESSIBLE for NONE breaks low-half conventions and hides the distinction from real permission errors. Unmapping NONE loses data. Keeping areas inside core AddressSpace would allocate under MEMORY, which with_memory forbids. File-backed mapping is deferred because static Go does not use it.

## Consequences

Frame exhaustion on first access ends the program with page fault rather than ENOMEM, like overcommit. check_user materializes an entire buffer before copying; read is therefore capped at 64 KB. At this step areas/tables are single-threaded; clone must lock faults/check-copy/unmap in a shared space. No copy-on-write without fork.
