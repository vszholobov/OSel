---
description: OSel development stages from a kernel to Go Tetris, real hardware and optional networking.
---

# Roadmap

**Audience:** anyone tracking completed and future work.

**Summary:** run console [Go Tetris](https://github.com/vszholobov/Tetris) in QEMU, then from USB on a laptop. Programs are ordinary Linux builds using OSel's Linux call subset ([0005](decisions/0005-linux-abi-for-go.md)). Networking is optional. Work starts in QEMU, with hardware checks at milestones.

Active-stage design lives in `design/`. Closing a stage moves its details into architecture and decisions, following the [single-home rule](guides/writing-docs.md#single-home).

## Stage 1. Kernel — verified on the owner’s computer {#stage-1}

- [x] `bootloader` 0.11, UEFI and BIOS, framebuffer text ([0001](decisions/0001-bootloader-0.11-and-uefi.md)).
- [x] Interrupts, 100 Hz timer, PS/2 keyboard and idle `hlt` ([interrupts](architecture/core/interrupts.md), [keyboard](architecture/drivers/keyboard.md)).
- [x] Paging, heap and higher-half kernel ([memory](architecture/core/memory.md), [0006](decisions/0006-higher-half-kernel.md)).
- [x] Panic screen with fault address/registers and handlers for processor exceptions.
- [x] Interrupt-masking locks with deadlock reports ([locking](architecture/core/locking.md)).
- [x] Host/QEMU/CI tests and heap-leak checks ([testing](architecture/testing.md)).
- [ ] USB-writing script; currently [manual](../user/usb.md).
- [x] Real-computer boot and Tetris confirmed by the owner on 2026-10-14. Model and firmware mode are not recorded; Samsung-specific validation remains unconfirmed.

## Stage 2. Programs — done {#stage-2}

Completed in [PR 7](https://github.com/vszholobov/OSel/pull/7): static Go runs goroutines with `time.Sleep` and exits. See [programs](architecture/core/programs.md), [threads](architecture/core/threads.md) and [calls](reference/syscalls.md).

- [x] cpio ramdisk, storage interfaces and Linux errors ([0007](decisions/0007-storage-interfaces.md)).
- [x] Frame deallocation and leak checks ([0008](decisions/0008-frame-pool.md)).
- [x] Program address spaces ([0009](decisions/0009-shared-kernel-half.md)).
- [x] ELF loading, argv/envp/auxv ([0010](decisions/0010-static-elf-loader.md)).
- [x] Ring 3, GDT/TSS kernel stack and contained user exceptions ([0011](decisions/0011-user-entry.md)).
- [x] `syscall`/`sysret`, `write`/`exit`; assembly prints `Hello from ring 3!` ([0012](decisions/0012-syscall-entry-and-user-programs.md)).
- [x] User SSE ([0013](decisions/0013-fpu-state.md)).
- [x] Threads and timer preemption ([0015](decisions/0015-threads.md)).
- [x] `clone` and `futex`.
- [x] Go runtime Linux calls and hello world.
- [x] `nanosleep`, `clock_gettime`, simplified `epoll`/`eventfd`; [Go goroutines and timers test](architecture/core/programs.md#go).

## Stage 2.5. Child programs — done {#stage-2-5}

Go `os/exec` launches programs; see [processes](architecture/core/threads.md#process) and [calls](reference/syscalls.md).

- [x] PIDs, parents and child waits.
- [x] vfork-style `clone` then `execve`, preserving Go's thread clone.
- [x] Inherited descriptors, `CLOEXEC`, standard-stream remapping and launch pipes.
- [x] Linux wait/exit status, launch failures and leak-free teardown.
- [x] `/bin/clear` and `PATH=/bin`: Tetris uses `exec.Command("clear")`, `cmd.Stdout = os.Stdout`, `cmd.Run()` and resumes afterward.
- [x] Successful, failed and repeated launches tested in UEFI and BIOS.

## Stage 3. Tetris — verified on a real computer {#stage-3}

See [Tetris](architecture/tetris.md). The external clearing command is preserved. The game's [PR 16](https://github.com/vszholobov/Tetris/pull/16) adds single-player mode.

- [x] `/dev/tty`, `ioctl` modes, serial input and scheduler waits.
- [x] Tetris in ramdisk, started from the [shell](architecture/shell.md), returning to the prompt.
- [x] `help`, `ls`, `cat`, executable launch, external `clear` and repeated play.
- [x] CI keyboard input/output scenario through serial.
- [x] ANSI framebuffer sequences and the `Ж` glyph; the game field uses ASCII.
- [x] PS/2 input through `/dev/tty`; the owner confirmed real-computer game controls on 2026-10-14.

Hardware milestone: Tetris works on the owner’s computer; model and BIOS/UEFI mode are not recorded.

## Stage 4 (optional). Network and modern hardware {#stage-4}

Network Tetris is implemented and tested in QEMU with the repository's local server. See [networking](architecture/core/network.md), [Ethernet](architecture/drivers/ethernet.md) and [user instructions](../user/network.md).

- [x] PCI and adapter discovery; hardware drivers in `src/drivers/`.
- [x] Intel 82574L (`e1000e`) in QEMU with RX/TX DMA rings.
- [x] Realtek RTL8168 G/GU/H/M driver for Acer PCI ID `10ec:8168`, XID diagnostics; shared DMA, DHCP and gameplay tested through QEMU C+. Laptop PHY remains untested.
- [x] ICMP ping, DNS lookup and network/PCI diagnostics.
- [ ] Wi-Fi QCA6174 WPA2 on Acer: [design](design/wifi.md); PCI/D0/wake/reset confirmed on Acer; ROM early configuration added after a CE0 timeout, awaiting another hardware test.
- [x] smoltcp: DHCP, IPv4 and TCP.
- [x] Outgoing Linux sockets and Go's required `epoll` modes.
- [x] Ramdisk `/etc/hosts` and configurable client server address.
- [x] Actual local Tetris server: two players, movement, fields, ping/pong, exit and repeated launch without leaks, UEFI and BIOS.
- [ ] Physical Acer networking: PCI ID is known; XID, PHY startup and wired DHCP/gameplay still need checking. Samsung is outside the task.
- [ ] Connect to the public server after the owner starts it.
- [ ] Boot [Acer Nitro](../user/hardware/acer-nitro.md) through UEFI.

Deferred work is listed in [technical debt](debt.md).
