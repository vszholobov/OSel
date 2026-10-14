---
description: Why each program owns its lower-half page tables and shares kernel level-3 tables.
---

# 0009. Shared kernel half of address spaces

**Audience:** developers changing program memory or page-table ownership.

**Summary:** each `AddressSpace` owns its level-4 table and lower half. Upper-half entries refer to shared kernel level-3 tables prepared during initialization, so later kernel mappings appear in every program.

**Status:** accepted. **Date:** 2026-10-12.

## Context

Programs need isolated low addresses while continuing to use kernel code, physical-memory mapping, heap and stacks. Copying existing kernel tables would miss mappings added after a program was created.

## Decision

Preallocate level-3 tables for every empty upper-half level-4 entry before creating user spaces, roughly 250 pages. Entries use PRESENT|WRITABLE and never USER_ACCESSIBLE, so later mappings do not widen the level-4 flags. Copy only those entries into each new level-4 table. Kernel-only mappings may be changed in the shared tables. Each program owns and frees only its lower half and level-4 frame. prune_empty_tables never frees shared upper-half tables or tables outside the pool. map_range explicitly rolls back errors without FrameGuard; failure-at-each-allocation tests detect leaks. Hardware integration is provided by `UserSpace`; the ownership logic is host-tested in `osel-core/src/address_space.rs`.

## Alternatives considered

Copying all kernel tables wastes memory and misses later changes. Synchronizing level-4 entries needs an address-space registry and locked traversal; about 1 MB of preallocated tables is cheaper. x86_64 clean_up_addr_range can free a shared table and leave every space pointing at a released page. A FrameGuard holding a mutable pool borrow prevents map_to. A must-consume FrameGuard panicking in drop only detects leaks at runtime; failure-injection tests catch them in advance.

## Consequences

map_range zeros all pages, including the 16 MB heap at boot. No new upper-half level-4 entry can be introduced after initialization: every entry already exists. Mapping a foreign frame, such as a user framebuffer, in the lower half would make destroy panic; supporting it requires a nonowned-page marker. See [memory](../architecture/core/memory.md#address-spaces).
