---
description: Why open files have three ownership layers and devices are nodes in a /dev overlay.
---

# 0014. Descriptors and devices

Language: [Русский](../../../ru/developer/decisions/0014-file-descriptors.md) · [English](0014-file-descriptors.md)

**Audience:** developers adding file kinds, devices or file calls.

**Summary:** descriptors point to OpenDescription then FileLike. Shared position/access/status belong to the description; FD_CLOEXEC belongs to the descriptor. WithDevices overlays CharDevice nodes in /dev. See [files](../architecture/core/fs.md#open-files).

**Status:** accepted. **Date:** 2026-10-12.

## Context

Step 7 needs openat/read/close/fstat/null/tty. Go checks descriptors 0–2 with fcntl(F_GETFD), accepting only EBADF failures; ReadFile uses openat/fstat/read-to-zero and Stat uses newfstatat. Future threads share the table and eventfd/epoll are file objects. [0007](0007-storage-interfaces.md) requires opening devices from Inode.

## Decision

FileLike is the object; OpenDescription is a shared open with access, O_APPEND/O_NONBLOCK and offset; FdTable holds lowest-free numbers and per-fd CLOEXEC. get returns an Arc so blocking terminal reads do not hold the table. WithDevices creates dev/null/tty nodes and resolve returns to the wrapper on .. to root. fs::file_for uses rdev, not spelling. open_access tests read-only Linux error order. Standard descriptors 0–2 provide terminal read/write.

## Alternatives considered

Comparing special device path strings fails equivalent spellings such as //dev/tty or /dev/./null. Device entries in cpio require runner/parser support despite kernel-known /dev, so are deferred. Descriptor-local access/status would make fcntl(F_SETFL)/dup incompatible.

## Consequences

New devices need a DEVICES row and fs::file_for branch. eventfd/epoll/pipe/sockets implement FileLike without inodes. The ramdisk's dev directory is hidden.
