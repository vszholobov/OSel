---
description: Acer Nitro UEFI boot, Secure Boot, BitLocker preparation and F12 boot menu for OSel.
---

# Acer Nitro

Language: [Русский](../../../ru/user/hardware/acer-nitro.md) · [English](acer-nitro.md)

**Audience:** anyone running OSel on an Acer Nitro.

**Summary:** use `OSel.uefi.img`. Disable Secure Boot after saving a BitLocker recovery key if encryption is enabled. Enable the F12 boot menu once.

{% note warning %}

OSel has not yet been tested on this laptop and the exact model is unknown. The following are typical Nitro 5 details and require verification. Check the bottom label for a model such as AN515-xx.

{% endnote %}

## Specifications {#specs}

| Item | Value | Verified |
|---|---|---|
| Product line | Acer Nitro, probably Nitro 5, Windows 10 | Owner report |
| Boot mode | UEFI; newer models often lack legacy BIOS mode | No |
| Secure Boot | Enabled | No |
| Keyboard | Built-in, probably PS/2; external USB needs firmware emulation | No |
| Network adapter | Usually Realtek or Killer | No |

## Preparation {#prepare}

### 1. BitLocker {#bitlocker}

{% note alert %}

If BitLocker encryption is enabled, changing Secure Boot can make Windows require a recovery key. Save it before changing settings.

{% endnote %}

1. Check **Start** → **Settings** → **Update & Security** → **Device encryption**, or Control Panel → BitLocker Drive Encryption.
2. If enabled, save the key from [your Microsoft account](https://account.microsoft.com/devicekey), writing it down or photographing it.

### 2. Firmware settings {#bios}

1. Press **F2** during power-on.
2. **Security** → **Set Supervisor Password**: set a password to unlock Secure Boot settings. You can remove it later.
3. **Boot** → **Secure Boot** → **Disabled**.
4. **Main** → **F12 Boot Menu** → **Enabled**.
5. **F10** → **Yes** to save.

## Boot {#boot}

1. Write `OSel.uefi.img` following [USB boot](../usb.md).
2. Power on and press **F12**.
3. Select the USB drive.

## Precautions {#risks}

OSel has no disk drivers and does not access the Windows disk or the laptop's NVMe storage. If this is a work laptop, begin with [Samsung NP530U4B](samsung-np530u4b.md), which needs fewer settings changes. On a hang, hold power for 5–10 seconds, remove the drive and reboot.

## Restore settings {#restore}

1. **F2** → **Boot** → **Secure Boot** → **Enabled**.
2. To remove an unneeded supervisor password: **Security** → **Set Supervisor Password**, enter the current password and leave the new one empty.
3. **F10** → **Yes**.

BitLocker may request the key again after restoring Secure Boot. See [recovery](../usb.md#recovery) to reuse the USB drive normally.
