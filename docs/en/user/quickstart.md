---
description: Start using an OSel image, play Tetris and return to your normal operating system.
---

# Quick start

Language: [Русский](../../ru/user/quickstart.md) · [English](quickstart.md)

**Audience:** users of ready-made OSel images.

**Summary:** download `OSel.uefi.img` or `OSel.bios.img` and `SHA256SUMS` from the same release. After boot, the `osel>` prompt appears; `tetris` starts the game.

## Get an image {#image}

Open [OSel releases](https://github.com/vszholobov/OSel/releases). [USB boot](usb.md#image) explains checksum verification, UEFI versus BIOS, and writing the image.

To run a BIOS image with QEMU already installed:

```shell
qemu-system-x86_64 -drive format=raw,file=OSel.bios.img -serial stdio -no-reboot
```

A UEFI image needs OVMF firmware. To build the kernel and use the runner, start with the [developer quick start](../developer/guides/quickstart.md).

## Shell {#shell}

After `OSel ready to go!`, the `osel>` prompt appears. Type `help` for commands, `ls /bin` for programs, `cat /etc/motd` for the welcome message, or `clear` to clear the screen.

## Tetris {#tetris}

Type `tetris`. Controls: `a` and `d` move left and right, `s` moves down, `q` and `e` rotate. `x`, Escape or Ctrl+C ends the game and returns to the prompt. Run it again with the same command. Network play is not supported yet.

## Exit {#exit}

In QEMU, close the window or stop it with Ctrl+C in the terminal that launched it. On a computer, power off or reboot and remove the USB drive. OSel does not modify internal drives; changes in the running system do not survive a reboot.

If a red screen appears, save a [crash report](crash-report.md).
