---
description: Completed single-player Tetris and remaining network-game design.
---

# Go Tetris

**Audience:** developers working on the game or future networking.

**Summary:** single-player mode is in [Tetris PR 16](https://github.com/vszholobov/Tetris/pull/16). Current architecture, launch, external `clear` and OSel tests live in [Tetris](../architecture/tetris.md). The remaining plan is networking.

## Network version (stage 4)

Use `ws://` and `http://` without TLS, requiring TCP only. Put the server address in ramdisk `/etc/hosts` to avoid DNS and UDP. Go handles WebSocket and HTTP; the kernel transports bytes. Add PCI enumeration, a network driver (Intel `e1000e` in QEMU first), smoltcp, sockets and full `epoll`.

Wi-Fi is outside this stage: adapter-specific drivers, firmware loading, association and WPA2/WPA3 would exceed the rest of the system's scope. Wired networking is much simpler. Check the actual adapter in Windows Device Manager → Network adapters; see the [hardware pages](../../user/hardware/samsung-np530u4b.md).
