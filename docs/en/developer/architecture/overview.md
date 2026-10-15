---
description: OSel crates, module map, boot sequence and virtual address layout.
---

# Architecture overview

**Audience:** developers opening the code for the first time.

**Summary:** OSel is a no_std Rust x86_64 kernel booted by bootloader 0.11 in UEFI/BIOS. Kernel hardware code, host-tested osel-core logic and the host runner are separate. Every module below has an owning page.

## Crates {#crates}

| Crate | Location | Target | Purpose |
|---|---|---|---|
| osel | `src/`, `tests/` | x86_64-unknown-none | Kernel library/binary and separate integration kernels |
| osel-core | `osel-core/` | Host and kernel | Hardware-independent logic |
| osel-runner | `runner/` | Host | Archive/image generation, QEMU and result interpretation |
| User programs | `user/<name>/` | x86_64-unknown-none | Ring-3 assembly programs packaged in /bin |
| Go programs | `user-go/<name>/` | static linux/amd64 | Go runtime tests packaged in /bin |
| docs-check | `tools/docs-check/` | Host | Bilingual documentation/code consistency checks |

osel-core is a workspace member; runner and docs-check are excluded. Nested runner builds must not contend for the kernel target lock. user/ programs have independent workspaces and user/target.

## Module map {#modules}

Existing kernel module names include wordplay; keep their names when changing code.

| Module | Location | Responsibility | Owner |
|---|---|---|---|
| `drivers` | `src/drivers/mod.rs` | hardware drivers; existing names are re-exported from the root | [Ethernet](drivers/ethernet.md) |
| `pci` | `src/drivers/pci.rs` | PCI configuration, BAR, bus mastering | [Ethernet](drivers/ethernet.md) |
| `e1000e` | `src/drivers/e1000e.rs` | Intel 82574L, RX/TX DMA | [Ethernet](drivers/ethernet.md) |
| `dma` | `src/drivers/dma.rs` | shared DMA pages | [Ethernet](drivers/ethernet.md) |
| `ethernet` | `src/drivers/ethernet.rs` | Intel/Realtek dispatch for smoltcp | [Ethernet](drivers/ethernet.md) |
| `rtl8168` | `src/drivers/rtl8168.rs` | Realtek G/GU/H/M and C+, DMA and PHY | [Ethernet](drivers/ethernet.md) |
| `net` | `src/net.rs` | DHCP, TCP, Linux sockets, service thread | [Networking](core/network.md) |
| `avganec` | `src/drivers/avganec.rs` | Framebuffer console, print macros and panic screen | [Details](drivers/console.md) |
| `cereal` | `src/drivers/cereal.rs` | COM1 serial and emergency output | [Details](drivers/serial.md) |
| `keyboard` | `src/drivers/keyboard.rs` | PS/2 initialization, scan-code decoding, input buffers and diagnostics | [Details](drivers/keyboard.md) |
| `interrupts` | `src/interrupts.rs` | IDT, exceptions, PIC, timer and spurious IRQs | [Details](core/interrupts.md) |
| `gdt` | `src/gdt.rs` | GDT, user selectors, TSS and IST stacks | [Details](core/interrupts.md) |
| `fpu` | `src/fpu.rs` | x87/SSE initialization, saved state and clean defaults | [Details](core/user-mode.md#fpu) |
| `syscall` | `src/syscall.rs` | Linux call numbers, entry, arguments, error encoding and safe return | [Details](core/user-mode.md#syscall), [Details](../reference/syscalls.md) |
| `scheduler` | `src/scheduler.rs` | Threads, processes, switching and timer preemption | [Details](core/threads.md) |
| `user` | `src/user.rs` | Ring-3 launch and exception return | [Details](core/user-mode.md) |
| `time` | `src/time.rs` | Monotonic/realtime clocks and deadline waits | [Details](core/threads.md#time) |
| `futex` | `src/futex.rs` | Address-keyed waiting and wakeup | [Details](core/threads.md#futex) |
| `poll` | `src/poll.rs` | eventfd counters, epoll observation and waits | [Details](core/fs.md#epoll) |
| `pipe` | `src/pipe.rs` | Bounded pipe buffers, endpoints, EOF/EPIPE and blocking waits | [Details](core/fs.md#pipes) |
| `security` | `src/security.rs` | Adversarial ring-3 isolation tests | [Details](core/security.md) |
| `memory` | `src/memory.rs` | Address layout, page tables, frame allocation and UserSpace | [Details](core/memory.md) |
| `program` | `src/program.rs` | Filesystem program loading and main-thread stack | [Details](core/programs.md) |
| `shell` | `src/shell.rs` | Bounded line editor, command parsing, program launch and terminal restoration | [Details](shell.md) |
| `fs` | `src/fs.rs` | Ramdisk root, device integration and file lookup | [Details](core/fs.md) |
| `tty` | `src/tty.rs` | Shared terminal, PS/2/COM1 input, UTF-8 output and wait queues | [Details](core/fs.md#open-files) |
| `allocator` | `src/allocator.rs` | Kernel heap and global allocator | [Details](core/memory.md#heap) |
| `sync` | `src/sync.rs` | Named spinlocks, interrupt masking and deadlock reports | [Details](core/locking.md) |
| `qemuno` | `src/qemuno.rs` | QEMU exit through isa-debug-exit | [Details](testing.md) |
| `testosterone` | `src/testosterone.rs` | Kernel test framework, expected panic and leak checks | [Details](testing.md) |
| `net` | `osel-core/src/net.rs` | Linux IPv4, PCI address / BAR parsing | [Ethernet](drivers/ethernet.md) |
| `rtl8168` | `osel-core/src/rtl8168.rs` | Realtek XID, descriptors and bounded PHY initialization | [Ethernet](drivers/ethernet.md) |
| `console` | `osel-core/src/console.rs` | Text rendering into byte buffers | [Details](drivers/console.md) |
| `shell` | `osel-core/src/shell.rs` | Bounded line editor, command parsing, program launch and terminal restoration | [Details](shell.md) |
| `font` | `osel-core/src/font.rs` | Built-in 8×16/16×32 rasters and glyph selection | [Details](drivers/console.md#font) |
| `keyboard` | `osel-core/src/keyboard.rs` | PS/2 initialization, scan-code decoding, input buffers and diagnostics | [Details](drivers/keyboard.md) |
| `ps2` | `osel-core/src/ps2.rs` | Bounded i8042 initialization with ACK/RESEND | [Details](drivers/keyboard.md) |
| `pit` | `osel-core/src/pit.rs` | Timer divisor, PIC masks and spurious detection | [Details](core/interrupts.md#pic-pit) |
| `memory` | `osel-core/src/memory.rs` | Address layout, page tables, frame allocation and UserSpace | [Details](core/memory.md) |
| `address_space` | `osel-core/src/address_space.rs` | Private level-4/lower-half tables with a shared kernel half | [Details](core/memory.md#address-spaces) |
| `vma` | `osel-core/src/vma.rs` | Nonoverlapping permission areas and range operations | [Details](core/memory.md#areas) |
| `mmap` | `osel-core/src/mmap.rs` | Lazy anonymous mappings and memory-call semantics | [Details](core/memory.md#areas) |
| `clock` | `osel-core/src/clock.rs` | Tick/timespec and CMOS-to-Unix conversions | [Details](core/threads.md#time) |
| `sleeper` | `osel-core/src/sleeper.rs` | Deadline queue | [Details](core/threads.md#time) |
| `futex` | `osel-core/src/futex.rs` | Address-keyed waiting and wakeup | [Details](core/threads.md#futex) |
| `poll` | `osel-core/src/poll.rs` | eventfd counters, epoll observation and waits | [Details](core/fs.md#epoll) |
| `pipe` | `osel-core/src/pipe.rs` | Bounded pipe buffers, endpoints, EOF/EPIPE and blocking waits | [Details](core/fs.md#pipes) |
| `terminal` | `osel-core/src/terminal.rs` | Linux termios, bounded input, canonical mode and echo | [Details](core/fs.md#open-files) |
| `frames` | `osel-core/src/frames.rs` | FrameSource and checked LIFO FramePool | [Details](core/memory.md#frames) |
| `heap` | `osel-core/src/heap.rs` | FixedSizeBlockAllocator and cache management | [Details](core/memory.md#heap) |
| `exception` | `osel-core/src/exception.rs` | Decoded exception reports and allocation-free FixedBuf | [Details](core/interrupts.md#exceptions) |
| `sync` | `osel-core/src/sync.rs` | Named spinlocks, interrupt masking and deadlock reports | [Details](core/locking.md) |
| `errno` | `osel-core/src/errno.rs` | Linux Errno and KResult | [Details](core/fs.md) |
| `block` | `osel-core/src/block.rs` | BlockDevice, MemBlockDevice and unaligned byte reads | [Details](core/fs.md#layers) |
| `vfs` | `osel-core/src/vfs.rs` | FileSystem/Inode, metadata and path resolution | [Details](core/fs.md#resolve) |
| `elf` | `osel-core/src/elf.rs` | Static ELF64 parser, ProgramMemory loader and ElfBuilder | [Details](core/programs.md#elf) |
| `initial_stack` | `osel-core/src/initial_stack.rs` | argv/envp/auxv initial stack | [Details](core/programs.md#initial-stack) |
| `syscall` | `osel-core/src/syscall.rs` | Linux call numbers, entry, arguments, error encoding and safe return | [Details](../reference/syscalls.md) |
| `fpu` | `osel-core/src/fpu.rs` | x87/SSE initialization, saved state and clean defaults | [Details](core/user-mode.md#fpu) |
| `sched` | `osel-core/src/sched.rs` | SchedPolicy, RoundRobin and guarded StackSlots | [Details](core/threads.md) |
| `trap` | `osel-core/src/trap.rs` | Shared TrapFrame and user exception names | [Details](core/user-mode.md#trap-frame) |
| `user_mem` | `osel-core/src/user_mem.rs` | Range and page-permission checks for user memory | [Details](core/user-mode.md#user-mem) |
| `cpio` | `osel-core/src/cpio.rs` | CpioFs and CpioBuilder | [Details](core/fs.md#cpio) |
| `devices` | `osel-core/src/devices.rs` | /dev overlay and device numbers | [Details](core/fs.md#devices) |
| `file` | `osel-core/src/file.rs` | FileLike, OpenDescription, FdTable, open flags and Linux stat | [Details](core/fs.md#open-files) |

## Boot sequence {#boot}

1. bootloader loads the kernel ELF in the upper half, maps physical memory/framebuffer and enters kernel_main in `src/main.rs`. BOOTLOADER_CONFIG lives in `src/lib.rs`.
2. avganec::init takes the framebuffer; COM1 prints the version/commit wake-up banner. build.rs embeds the commit; image verification checks it against the release.
3. osel::init runs exactly once, in order: mask interrupts; GDT/TSS; x87/SSE; IDT; syscall MSRs; memory/lower-half cleanup/IST guards/frame pool/shared upper tables; heap; [RAM console cells](drivers/console.md#scrolling); frame tracking; root ramdisk with /dev; scheduler boot/idle threads; PS2 setup then PIC/PIT; clock/futex/poll queues; enable interrupts.
4. Print the screen banner, then /etc/motd, run /bin/hello, print OSel ready to go!, then enter the [shell](shell.md). tetris starts the game and returns to the prompt.

osel::boot_info() retains the bootloader memory map, ramdisk and ACPI information after initialization.

## Address space {#address-space}

| Region | Range |
|---|---|
| Program-private memory | Lower half below USER_END |
| Kernel, boot stack/information, framebuffer, ramdisk, physical mapping | KERNEL_DYNAMIC_START..=KERNEL_DYNAMIC_END |
| 16 MB kernel heap | HEAP_START |
| Guarded 64 KB thread kernel stacks | STACKS_START |

See [memory](core/memory.md) for ownership and pitfalls.

## Decisions {#decisions}

See [our decisions](../decisions/index.md) for bootloader, host tests, higher-half memory, storage, frames, shared address spaces, ELF, user entry, calls, FPU, descriptors and threads.

_Checked against code: 2026-10-15, fix-console-scroll branch._

After terminal setup, `net::init` discovers PCI and starts the network thread before enabling interrupts. See [networking](core/network.md).
