---
description: Why storage uses BlockDevice, FileSystem and Inode, with a cpio ramdisk built by the runner.
---

# 0007. Storage interfaces and cpio ramdisk

Language: [Русский](../../../ru/developer/decisions/0007-storage-interfaces.md) · [English](0007-storage-interfaces.md)

**Audience:** developers adding files, devices or storage.

**Summary:** `BlockDevice` handles numbered blocks, `FileSystem` exposes a root, and `Inode` handles nodes. The ramdisk is a memory block device containing cpio “newc”; runner builds it from `ramdisk/`. Errors use Linux numbers from the start.

**Status:** accepted. **Date:** 2026-10-12.

## Context

Static Go programs need files. `bootloader` 0.11 can load a ramdisk alongside the kernel. Terminal devices, sockets and eventual disk storage should not force every file consumer to change.

## Decision

- Separate storage interfaces: `Inode` provides `stat`, `read_at`, `lookup`, `parent` and `read_dir`. Default methods return Linux errors, so nodes implement only supported operations.
- Mount `CpioFs` over `BlockDevice`, rather than a raw byte slice, so a disk-backed implementation can replace it later.
- Use cpio “newc”: a small parser and a format also used for Linux initramfs.
- Build the archive from a repository directory, without binary archives in Git. `CpioBuilder` lives beside the reader in `osel-core`; runner depends on that crate, and round-trip tests check the shared format.
- Use Linux-numbered `Errno` so system calls can return errors directly.
- Put all logic except the bootloader ramdisk address in host-tested `osel-core`, following [0002](0002-osel-core-host-tests.md).

## Alternatives considered

ustar requires extensions for names longer than 100 bytes. Parsing a raw slice directly would bypass the future block-device interface. Embedding program bytes with `include_bytes!` would still leave Go and Tetris needing a ramdisk. A ready-made archive or an archive-path environment variable would put unreadable binaries in Git and risk drifting from their source; OSEL_RAMDISK therefore selects a directory. Page/dentry caches, mounts, permission enforcement and writes are deferred until a task requires them: stages 2/3 need one read-only root.

## Consequences

Every normal/test kernel receives the same archive; per-test ramdisks are future work. Descriptor tables store FileLike: ordinary files/devices open from Inode, while pipes/eventfd/sockets implement FileLike independently. resolve already accepts cwd and Inode provides parent, so openat/chdir do not require redesign. New filesystems implement FileSystem/Inode; new disks implement BlockDevice. See [filesystem architecture](../architecture/core/fs.md).
