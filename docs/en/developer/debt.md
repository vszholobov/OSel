---
description: Deliberately deferred OSel work and links to the owning subsystem pages.
---

# Technical debt

**Audience:** developers choosing improvements or encountering a known limitation.

**Summary:** one row per deferred decision, with its reason and owning page. Remove completed items; details belong to subsystem pages.

| Work | Why deferred | Details |
|---|---|---|
| Backspace across lines | Needs terminal/editor work | [Console](architecture/drivers/console.md) |
| Separate #GP stack | Do not move page faults to IST: they need nesting | [Interrupts](architecture/core/interrupts.md) |
| CI fmt/clippy | Enabling formatting changes the whole codebase; separate PR | [CI](guides/toolchain-and-ci.md) |
| KVM in CI | Faster but changes timer behavior; retain software-emulated coverage | [Tests](architecture/testing.md#ci) |
| Cargo test name filters | Test kernels cannot select names | [Runner](reference/runner.md#test-args) |
| Heap growth beyond 16 MB | Requires taking MEMORY under ALLOCATOR | [Memory](architecture/core/memory.md#heap), [locking](architecture/core/locking.md#pitfalls) |
| Per-test ramdisk | All kernels currently receive the same archive/programs | [Files](architecture/core/fs.md) |
| cpio symlinks/device files | Only files, directories and hard links are supported | [cpio](architecture/core/fs.md#cpio) |
| Inode cache | lookup creates a fresh object; cheap for ramdisk, not disks | [Files](architecture/core/fs.md#pitfalls) |
| Missing/corrupt ramdisk boot test | Runner always packs a valid archive | [Files](architecture/core/fs.md#invariants) |
| Signal delivery | rt_sigaction/tgkill are stubs; nil access terminates Go instead of panicking | [Go](architecture/core/programs.md#go), [calls](reference/syscalls.md) |
| Sleep accuracy | 100 Hz ticks give 10 ms granularity | [Threads](architecture/core/threads.md#time) |
| Cryptographic getrandom | Current splitmix64/tick seed is not secure entropy | [Calls](reference/syscalls.md) |
| File mmap | Static Go does not use it; returns ENODEV | [Memory](architecture/core/memory.md#areas), [0016](decisions/0016-lazy-anonymous-memory.md) |
| Reload ELF pages after MADV_DONTNEED | Loaded segments are anonymous; reads return zeros | [Memory](architecture/core/memory.md#areas) |
| Materialize syscall buffers during copying | check_user currently maps the whole buffer; read is limited to 64 KB | [Memory](architecture/core/memory.md#areas) |
| Linux output/exit comparison and Go strace | Planned, not automated | [Future tests](architecture/testing.md#future-checks) |
| Deterministic QEMU -icount time | No runner mode yet; tests tolerate timing variance | [Future tests](architecture/testing.md#future-checks) |
| USB-writing script | Manual until real-hardware verification | [USB](../user/usb.md) |

Networking defers other Ethernet adapters, IRQ/MSI, DNS, listening sockets and detached FIN/TIME_WAIT queues; see [networking restrictions](architecture/core/network.md#pitfalls).
