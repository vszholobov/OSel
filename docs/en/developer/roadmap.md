---
description: OSel development stages from a kernel to Go Tetris, real hardware and optional networking.
---

# Roadmap

**Audience:** anyone tracking completed and future work.

**Summary:** run console [Go Tetris](https://github.com/vszholobov/Tetris) in QEMU, then from USB on a laptop. Programs are ordinary Linux builds using OSel's Linux call subset ([0005](decisions/0005-linux-abi-for-go.md)). Networking is optional. Work starts in QEMU, with hardware checks at milestones.

Active-stage design lives in `design/`. Closing a stage moves its details into architecture and decisions, following the [single-home rule](guides/writing-docs.md#single-home).

## Stage 1. Kernel — implemented, hardware check pending {#stage-1}

- [x] `bootloader` 0.11, UEFI and BIOS, framebuffer text ([0001](decisions/0001-bootloader-0.11-and-uefi.md)).
- [x] Interrupts, 100 Hz timer, PS/2 keyboard and idle `hlt` ([interrupts](architecture/core/interrupts.md), [keyboard](architecture/drivers/keyboard.md)).
- [x] Paging, heap and higher-half kernel ([memory](architecture/core/memory.md), [0006](decisions/0006-higher-half-kernel.md)).
- [x] Panic screen with fault address/registers and handlers for processor exceptions.
- [x] Interrupt-masking locks with deadlock reports ([locking](architecture/core/locking.md)).
- [x] Host/QEMU/CI tests and heap-leak checks ([testing](architecture/testing.md)).
- [ ] USB-writing script; currently [manual](../user/usb.md).
- [ ] Boot [Samsung NP530U4B](../user/hardware/samsung-np530u4b.md) from USB and print pressed keys.

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

## Stage 3. Tetris — works in QEMU, laptop pending {#stage-3}

See [Tetris](architecture/tetris.md). The external clearing command is preserved. The game's [PR 16](https://github.com/vszholobov/Tetris/pull/16) adds single-player mode.

- [x] `/dev/tty`, `ioctl` modes, serial input and scheduler waits.
- [x] Tetris in ramdisk, started from the [shell](architecture/shell.md), returning to the prompt.
- [x] `help`, `ls`, `cat`, executable launch, external `clear` and repeated play.
- [x] CI keyboard input/output scenario through serial.
- [x] ANSI framebuffer sequences and the `Ж` glyph; the game field uses ASCII.
- [x] PS/2 input through `/dev/tty`; real laptop keyboard remains unverified.

Hardware milestone: play Tetris on Samsung from USB.

## Stage 4 (optional). Network and modern hardware {#stage-4}

See [network design](design/tetris.md).

- [ ] PCI enumeration and network adapter discovery.
- [ ] Intel `e1000e` in QEMU, then the laptop adapter.
- [ ] smoltcp stack, DHCP and TCP.
- [ ] Sockets and full `epoll`.
- [ ] Server address in ramdisk `/etc/hosts`; play against the real server.
- [ ] UEFI boot on [Acer Nitro](../user/hardware/acer-nitro.md).

Deferred work is listed in [technical debt](debt.md).
