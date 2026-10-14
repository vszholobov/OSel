---
description: Virtual layout, physical frame pool, page tables, private program spaces, lazy mmap and kernel heap.
---

# Memory

Language: [Русский](../../../../ru/developer/architecture/core/memory.md) · [English](memory.md)

**Audience:** developers changing pages, address spaces or heap allocation.

**Summary:** kernel/bootloader mappings occupy the upper half. Programs own private lower halves and share the kernel half. with_memory guards page tables and the frame pool. Freed frames are reused LIFO and invalid frees panic. The heap is fixed at 16 MB; anonymous user pages appear on first permitted access.

## Layout {#layout}

Constants live in `osel-core/src/memory.rs`.

| Region | Range | Contents |
|---|---|---|
| Programs | 0..USER_END | Kernel lower half is empty; each program has its own. mmap chooses below MMAP_TOP = USER_END − 1 GB; main stack is above |
| Noncanonical hole | USER_END..KERNEL_DYNAMIC_START | Invalid addresses |
| Bootloader region | KERNEL_DYNAMIC_START..=KERNEL_DYNAMIC_END | Kernel ELF/stack/boot info, framebuffer and physical mapping |
| Kernel heap | HEAP_START..HEAP_START + HEAP_SIZE | 16 MB in a separate level-4 entry |
| Kernel stacks | STACKS_START in `src/scheduler.rs` | 64 KB slots with guards in a separate level-4 entry |

BOOTLOADER_CONFIG in `src/lib.rs` sets dynamic_range_start/end and maps all physical memory with Mapping::Dynamic.

## Behavior {#how}

### Page tables {#page-table}

memory::init in `src/memory.rs` runs once through osel::init. Read CR3 with the physical offset; clear all 256 lower-half entries and flush TLB; retain the memory map; build OffsetPageTable; unmap IST guards; create FramePool; create level-3 tables for every empty upper-half entry; store mapper/pool/kernel_level_4 under MEMORY. After heap initialization, enable_frame_tracking permits freeing.

Use with_memory for mapper/frame_allocator access with interrupts masked.

### Physical frames {#frames}

FrameSource in `osel-core/src/frames.rs` combines FrameAllocator/FrameDeallocator with used_frames and can_free. FramePool implements it; see [0008](../../decisions/0008-frame-pool.md). RegionFrameAllocator supplies fresh frames in ascending order above LOW_MEMORY_END (1 MB), requiring the complete frame in Usable memory and outside every other region. It accepts unsorted/overlapping maps and never frees.

Released frames store the next-pointer in their first eight bytes through the physical mapping, with zero ending the list. Allocate LIFO before fresh frames. A bitmap marks frames in the free list; an issued usable frame below the allocator cursor with a clear bit is owned. Double/foreign release panics with `frame … is not handed out: double free or a foreign frame`.

Before heap/tracking, allocation works but release panics. enable_frame_tracking allocates the bitmap before taking MEMORY; the initially empty bitmap also covers previously issued heap frames. MAX_TRACKED_BYTES bounds the pool to 64 GB above its bottom, with a bitmap no larger than 2 MB. used_frames counts outstanding pages; count_frames/total_usable_frames report capacity.

### Range mapping {#map-range}

map_range in `osel-core/src/memory.rs` allocates and zeros fresh pages. MapRangeError is InvalidRange for overflow/noncanonical crossing, PermissionDenied for wrong-half USER_ACCESSIBLE or user GLOBAL, and Map for map_to failures (existing mappings or exhausted frames).

On failure roll back every newly mapped/data/table frame, including the failing allocation, and prune empty tables. unmap_range frees pages and empty tables by traversing tables rather than iterating every page, making huge empty ranges cheap. The caller flushes TLB after unmapping or mapping failure.

prune_empty_tables preserves bootloader-owned tables, huge-page entries and shared upper-half level-3 tables.

### Address spaces {#address-spaces}

AddressSpace in `osel-core/src/address_space.rs` owns level 4 and its lower half. Copy 256 upper-half entries referencing level-3 tables precreated with PRESENT|WRITABLE, so later kernel mappings appear in every space ([0009](../../decisions/0009-shared-kernel-half.md)). map/unmap accept only user pages. destroy walks children before parents, freeing all low-half pages/tables then level 4. Lower-half huge pages panic because OSel does not create them.

UserSpace in `src/memory.rs` wraps this ownership. new copies kernel entries; map/unmap/translate/page_flags provide access. Active-space unmaps and failed maps flush TLB. write_bytes/read_bytes use the physical mapping and ignore user rights for loading; user pointers instead require [checked copies](user-mode.md#user-mem). activate writes CR3; activate_kernel_space restores the kernel table; drop destroys the inactive space.

### Areas and demand pages {#areas}

Mappings in `osel-core/src/mmap.rs` owns a VmaSet from `osel-core/src/vma.rs`: nonoverlapping page ranges with PROT permissions, merging identical neighbors. UserSpace stores these beside its tables ([0016](../../decisions/0016-lazy-anonymous-memory.md)).

mmap initially reserves an empty area. user_fault/map_on_demand calls UserSpace::fault for missing low-half pages. Allocate zeroed memory only if the area permits access; resume the same instruction, otherwise terminate with a fault. Any nonzero protection allows x86 reads, write needs PROT_WRITE and execute PROT_EXEC. PROT_NONE Go reserves consume no frames.

flags_of derives leaf rights; no execute permission sets NO_EXECUTE. All lower-level user tables use USER_TABLE_FLAGS = PRESENT|WRITABLE|USER_ACCESSIBLE so leaf permissions alone control access. A populated PROT_NONE leaf retains its physical address without PRESENT and with BIT_9/PROT_NONE_FLAGS. mprotect restores rights/data; unmap/madvise/destroy inspect nonempty entries rather than PRESENT to free it.

Anonymous mmap takes a free hint or highest space below MMAP_TOP; MAP_FIXED replaces existing ranges/pages. munmap removes areas/pages. mprotect validates full coverage before changing anything. MADV_DONTNEED/MADV_FREE remove physical pages but retain areas, so next access reads zeros. See [errors](../../reference/syscalls.md#calls).

ELF segments and stack are eagerly mapped areas via prot_of; guards are not areas. check_user/Mappings::fault_in materialize permitted syscall-buffer pages before checking leaf flags. Fresh read buffers work; PROT_NONE returns EFAULT. PageTables operations acquire MEMORY separately because area vectors allocate and allocation inside with_memory is forbidden. Active changes flush TLB.

### Heap {#heap}

init_heap in `src/allocator.rs` maps HEAP_SIZE with PRESENT|WRITABLE|NO_EXECUTE and initializes FixedSizeBlockAllocator in `osel-core/src/heap.rs`. KernelAllocator takes ALLOCATOR. Requests through 2048 bytes choose BLOCK_SIZES 8,16,…,2048 by max(size,align), caching released blocks by size. Larger requests use linked_list_allocator. If combined cache/free capacity could satisfy a failed request, return cached blocks and retry. Exhaustion returns null then normal allocation panic. in_use excludes cache for leak checks.

## Code {#code}

`osel-core/src/memory.rs`: layout, RegionFrameAllocator, map_range/unmap_range/protect_range/prune_empty_tables and flags/errors. `osel-core/src/vma.rs`: VmaSet/Vma/protection. `osel-core/src/mmap.rs`: Mappings/PageTables/flags_of/prot_of/constants. `osel-core/src/frames.rs`: FrameSource/FramePool. `osel-core/src/address_space.rs`: AddressSpace/create_kernel_tables. `osel-core/src/heap.rs`: allocator/list_index. `src/memory.rs`: init/tracking/with_memory/usage/UserSpace/Tables/activation. `src/user.rs`: demand faults. `src/allocator.rs`: heap/globals. `src/lib.rs`: boot config.

## Invariants {#invariants}

Kernel lower half stays empty. All 256 upper-half level-4 entries remain fixed and their shared level-3 tables are never freed. Kernel pages are not USER_ACCESSIBLE and user pages are not GLOBAL. Intermediate user tables have full rights; leaf rights match their area. Drop of an active space panics. Never issue frames below 1 MB. with_memory neither reenters nor allocates. Initialize once. Free only pool-owned frames, never bootloader/kernel/device pages; do not access a frame after freeing because it belongs to the intrusive list.

## Locking {#locks}

MEMORY/ALLOCATOR are IrqSpinlock. IRQ handlers do not allocate. Only ring-3 page faults map pages, when no kernel lock is already held; UserSpace::fault takes MEMORY without heap allocation. Frame allocation/free uses no heap. Allocate tracking storage before the guard. Reentry panics; see [locking](locking.md).

## Pitfalls {#pitfalls}

Pool pages contain prior data/list links: zero user pages and page tables. x86_64 clean_up_addr_range can free shared/bootloader tables; use prune_empty_tables. UserSpace drop takes MEMORY and must not occur inside with_memory. Raw map_to tests must free created tables. Before tracking, map-range rollback cannot free; this only occurs on failing heap initialization, which ends boot.

Clear bootloader low-half transition/GDT mappings before reusing UEFI Usable pages. Memory maps may be unsorted/overlap: search all regions and avoid reserved overlap. Clamp broken maps to PHYS_ADDR_LIMIT (2^52) before creating PhysAddr. linked_list_allocator's minimum is 16 bytes, so cached eight-byte blocks count as 16. Return cache from high addresses downward to avoid quadratic hole-list walks with interrupts masked.

Caller flushes TLB after unmap/failure; active UserSpace does it. Page::range_inclusive can panic on the final lower-half page, so use the core pages iterator. Fresh successful map needs no flush, but explicit remapping does. map_to gives intermediate tables the first leaf's rights: use map_to_with_table_flags with full user rights or later widening mprotect fails. Missing read-only page write faults lack PRESENT, so distinguish them by area rights. Flush after widening mprotect too. Growing Vec requires old/new buffers simultaneously and cannot exceed about half the fixed heap.

## Tests {#tests}

Host tests cover LIFO/invalid frees/tracking limits and random model operations; shared spaces, zeroing, isolation, late kernel mappings, rollback at every allocation, destroy and pruning; unsorted/broken maps, full-half ranges, rights and PROT_NONE leaves; area insertion/merge/fixed replacement/coverage; mmap flags/hints/errors/demand pages/none data/madvise/copies with model comparison; heap alignment/cache/random load/release speed.

Kernel tests cover shared upper tables, no user access there, space isolation, late mappings, failure injection without leaks, freed/heap frame ownership, guards/TLB/zero unmaps and huge pages, plus global allocator alignment/OOM. Integration includes `tests/frame_exhaustion.rs`, `tests/double_free.rs`, `tests/frame_leak_detection.rs`, `tests/drop_active_space.rs`, `tests/heap_allocation.rs`, `tests/heap_stress.rs`, `tests/heap_oom.rs`, `tests/memory_reentrancy.rs` and `tests/write_protection.rs`.

Syscall memory tests exercise zero/faulted reservations, read/execute permissions, lazy execution, fixed/unmap stack pages, widening mprotect, PROT_NONE data, madvise, huge unmaps, checked copies/EFAULT, ELF/stack areas and 30 leak-free programs.

## Decisions {#decisions}

[0006](../../decisions/0006-higher-half-kernel.md), [0002](../../decisions/0002-osel-core-host-tests.md), [0008](../../decisions/0008-frame-pool.md), [0009](../../decisions/0009-shared-kernel-half.md), [0016](../../decisions/0016-lazy-anonymous-memory.md).

_Checked against code: 2026-10-13, commit 2c68d3f._
