---
description: OSel user documentation — ready-made images, booting, hardware and crash reports.
---

# User documentation

**Audience:** anyone who wants to run OSel and play Tetris.

**Summary:** download an image, boot it in QEMU or from USB, and type `tetris` in the shell. You do not need to build the kernel or install Rust or Go.

## Getting started {#start}

- [Quick start](quickstart.md): images, shell commands and game controls.
- [USB boot](usb.md): image selection, checksum verification and writing the drive.
- [Crash reports](crash-report.md): information to attach when reporting a failure.

## Hardware {#hardware}

- [Samsung NP530U4B](hardware/samsung-np530u4b.md).
- [Acer Nitro](hardware/acer-nitro.md).

These pages help prepare a boot attempt; a listed machine is not necessarily verified with OSel. Each page lists known capabilities and outstanding checks.

To change the code, use the [developer documentation](../developer/index.md).

[Network, PCI and Wi-Fi diagnostics](diagnostics.md).
