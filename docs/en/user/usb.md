---
description: Write an OSel image to a USB drive and boot a real computer without damaging existing data.
---

# USB boot

Language: [Русский](../../ru/user/usb.md) · [English](usb.md)

**Audience:** anyone running OSel on a real computer.

**Summary:** download an image from GitHub Releases, verify its checksum, write the whole USB drive and select it in the boot menu. Rust, Go and a project build are unnecessary. OSel does not access internal disks; remove the USB drive to return to your usual system. Double-check the destination disk before writing.

{% note alert %}

Writing an image erases the **entire** selected disk. Selecting an internal disk destroys its data. There is no script to check your selection: you must identify the drive yourself.

{% endnote %}

## 0. Prepare the computer {#prepare}

Do this before writing the drive or changing firmware settings:

- With Windows BitLocker encryption, save the recovery key before disabling Secure Boot. Windows may require it next boot. See [Acer Nitro](hardware/acer-nitro.md).
- For UEFI boot, disable Secure Boot: the OSel bootloader is unsigned. See the machine-specific [Acer Nitro](hardware/acer-nitro.md) instructions.
- On Samsung, disable Fast BIOS Mode so USB devices appear: [Samsung NP530U4B](hardware/samsung-np530u4b.md).

Record your changes so you can [restore them](#recovery) later.

## 1. Download and verify the image {#image}

| Boot mode | Image |
|---|---|
| UEFI, typical of Windows 8 and newer computers | `OSel.uefi.img` |
| Legacy BIOS or compatibility mode (CSM), including 2012–2013 Samsung laptops | `OSel.bios.img` |

{% note alert %}

Some 2012–2013 Samsung laptops, including NP530U4B, could stop booting after UEFI settings were written. Use only `OSel.bios.img` in BIOS/CSM mode. See [Samsung NP530U4B](hardware/samsung-np530u4b.md).

{% endnote %}

Open [GitHub Releases](https://github.com/vszholobov/OSel/releases) and choose a release whose **Assets** contain `OSel.uefi.img`, `OSel.bios.img` and `SHA256SUMS`. Older releases may lack images. Download the required `.img` and `SHA256SUMS` from the same release to one folder. **Source code** archives are not boot images.

Images are uncompressed and include the shell, Tetris, external `clear` and the font license. Compute the downloaded file's SHA-256, substituting `OSel.bios.img` for BIOS:

{% list tabs %}

- macOS

  ```shell
  shasum -a 256 OSel.uefi.img
  ```

- Linux

  ```shell
  sha256sum OSel.uefi.img
  ```

- Windows (PowerShell)

  ```powershell
  Get-FileHash .\OSel.uefi.img -Algorithm SHA256
  ```

{% endlist %}

Compare all 64 characters with that file's entry in `SHA256SUMS`; letter case does not matter. If they differ, download again and do not write the image. With both images downloaded, run `shasum -a 256 -c SHA256SUMS` on macOS or `sha256sum -c SHA256SUMS` on Linux; both files must report `OK`.

### Optional: build from source {#build-image}

Install the tools in the [developer quick start](../developer/guides/quickstart.md#requirements), then run one command in the OSel folder. QEMU runs without a window and exits after the kernel boots:

```shell
OSEL_EXPECT="OSel ready to go!" cargo run --release --locked
OSEL_BOOT=bios OSEL_EXPECT="OSel ready to go!" cargo run --release --locked
```

Images appear in `target/x86_64-unknown-none/release/`. Copy the required `.img` to your working folder. Tetris requires Go and the initialized submodule.

## 2. Write the USB drive {#write}

Use a drive of at least 64 MB. All its contents will be erased.

{% list tabs %}

- macOS

  1. Run `diskutil list`, insert the drive, and run it again. The new disk is the USB drive. Look for `external, physical` and its capacity. Never select `internal`. The number `4` below is an example; substitute your actual disk number.
  2. Unmount the drive:

     ```shell
     diskutil unmountDisk /dev/disk4
     ```

  3. Write the image; the `r` in `rdisk4` makes writing faster:

     ```shell
     sudo dd if=OSel.uefi.img of=/dev/rdisk4 bs=4m
     ```

  4. If macOS says the disk is unreadable, select **Ignore** or **Eject**, never **Initialize**: initialization erases the image.
  5. Eject with `diskutil eject /dev/disk4`.

- Linux

  1. Run `lsblk -d -o NAME,SIZE,MODEL,TRAN` before and after insertion. The new drive has `usb` in `TRAN`. Substitute its name, such as `sdb`, for `sdX`.
  2. Unmount all mounted partitions with `sudo umount /dev/sdX*`.
  3. Write the whole disk (`/dev/sdX`, not `/dev/sdX1`):

     ```shell
     sudo dd if=OSel.uefi.img of=/dev/sdX bs=4M conv=fsync status=progress
     ```

- Windows

  Use [Rufus](https://rufus.ie) or [balenaEtcher](https://etcher.balena.io), selecting the image and USB drive carefully. In Rufus, choose whole-image DD mode.

{% endlist %}

## 3. Boot from USB {#boot}

1. Insert the drive into the powered-off computer.
2. Power on and repeatedly press the boot-menu key:

   | Manufacturer | Key |
   |---|---|
   | Lenovo, Dell, Acer | F12 |
   | Samsung | F10 |
   | HP | F9 or Esc |
   | ASUS | Esc or F8 |
   | MSI | F11 |

3. Select the USB drive. If missing, check [preparation](#prepare).

## Verify the boot {#check}

Expect `OSel X.Y.Z (commit …) woke up!`, `OSel ready to go!` and `osel>`. Version and commit must match the downloaded release. Try `help` or `tetris`; `x` exits the game.

If the keyboard does not work, press a few letters, wait 3–5 seconds and photograph `PS/2 init` and `KBD`. Report whether `t` changes, or send a short video. Diagnostics appear automatically; no command is needed. With working input, `kbd` repeats details. See [keyboard diagnostics](../developer/architecture/drivers/keyboard.md#diagnostics).

## Return to your usual system {#recovery}

Hold the power button for 5–10 seconds, remove the drive and power on. OSel cannot write internal disks, so the installed system remains unchanged.

After testing:

- Restore the firmware settings changed during [preparation](#prepare), including Secure Boot and Fast BIOS Mode. BitLocker may ask for the recovery key again.
- Reformat the whole USB drive for normal use. On macOS, select the device rather than a partition in Disk Utility and erase it. On Windows use Disk Management or Rufus; on Linux use a disk utility such as GNOME Disks.

## Common problems {#errors}

| Symptom | Action |
|---|---|
| USB drive absent from boot menu | Disable Fast BIOS Mode or Secure Boot and match the image to the boot mode |
| Blank screen | Check the required boot mode; respect the Samsung BIOS-only precaution |
| Red `OSel crashed :(` screen | Save a [crash report](crash-report.md) |
| Text appears but keyboard is silent | External USB keyboards require BIOS legacy USB emulation; try the built-in keyboard |
