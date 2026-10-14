---
description: Why the kernel and bootloader mappings occupy the upper address half and programs occupy the lower half.
---

# 0006. Higher-half kernel

**Audience:** developers changing virtual address layout or page tables.

**Summary:** kernel code, stack, boot information, framebuffer and physical-memory mapping occupy the upper half beginning at `0xffff_8000_0000_0000`; the heap follows. The lower half is for user programs. User access is forbidden above the boundary and required below it. See [memory](../architecture/core/memory.md).

**Status:** accepted. **Date:** 2026-10-11.

## Context

A low-half kernel is convenient before user programs exist but conflicts with independent program address spaces. `bootloader` 0.11 also leaves its GDT and transition-code pages mapped in the lower half.

## Decision

`BOOTLOADER_CONFIG` in `src/lib.rs` restricts dynamic mappings to `KERNEL_DYNAMIC_START..KERNEL_DYNAMIC_END` from `osel-core/src/memory.rs`. `HEAP_START` follows in a separate top-level entry. `memory::init` in `src/memory.rs` clears the bootloader's lower-half entries. `map_range` rejects a `USER_ACCESSIBLE` flag inconsistent with the address half.

## Alternatives considered

A low-half kernel interferes with programs. Keeping bootloader service pages is unsafe: UEFI marks their physical pages free, so they could be allocated again while retaining stale mappings.

## Consequences

Stage 2 constructs a private initially empty lower half and a shared upper half. A mistaken user-accessible map_range call at a kernel address cannot expose kernel tables.
