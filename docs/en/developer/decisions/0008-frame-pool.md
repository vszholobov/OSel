---
description: Why physical frames use FrameSource and a checked LIFO FramePool with leak detection.
---

# 0008. Physical frame pool

**Audience:** developers allocating or freeing physical pages.

**Summary:** `FrameSource` allocates and deallocates frames; `FramePool` wraps `RegionFrameAllocator`, reuses released frames in LIFO order and rejects double-free or foreign frames. Kernel tests check frame leaks.

**Status:** accepted. **Date:** 2026-10-12.

## Context

Programs must release pages and page tables. The original region allocator only handed pages out; repeated programs would exhaust memory. A small pool is sufficient before shared pages or copy-on-write.

## Decision

`FrameSource` abstracts the real pool and failing/test allocators. Fresh frames come from `RegionFrameAllocator`; free frames are returned first, with an intrusive list stored in the released pages themselves. LIFO improves locality and its order is tested. A bitmap records whether an already-issued usable frame is free; an initially empty bitmap also covers frames issued before tracking started. Tracking is enabled after heap setup. Invalid releases panic. `forbid_frame_leaks` compares usage around tests.

## Alternatives considered

A free list without a bitmap lets double release create a cycle whose failure appears far from the bug. Putting the bitmap in pool pages requires contiguous physical memory or an extra lookup layer without benefit while heap setup precedes freeing. Per-frame reference counts are deferred until sharing/copy-on-write; FrameSource permits extension. Silently ignoring foreign frames hides bugs because OSel destroys only pool-owned low-half pages. FrameGuard is deferred: holding a mutable pool borrow prevents using it for mapping, while taking MEMORY in drop deadlocks inside with_memory.

## Consequences

Returned pages are not zeroed. Tests must release their temporary page tables as well as data pages. The original decision named clean_up_addr_range; the current safe implementation is documented in [memory](../architecture/core/memory.md#pitfalls). Harness-free kernels must explicitly use testosterone::forbid_frame_leaks.
