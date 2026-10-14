---
description: Samsung NP530U4B boot mode, firmware settings, keyboard and OSel testing precautions.
---

# Samsung NP530U4B

Language: [Русский](../../../ru/user/hardware/samsung-np530u4b.md) · [English](samsung-np530u4b.md)

**Audience:** anyone booting OSel on this laptop.

**Summary:** the first target for real-hardware verification. Use `OSel.bios.img` in BIOS/CSM mode, disable Fast BIOS Mode once, then select the USB drive with F10.

{% note warning %}

OSel has not yet been booted on this laptop. Model specifications below require verification; update this page after the first boot.

{% endnote %}

## Specifications {#specs}

| Item | Value | Verified |
|---|---|---|
| Model | Samsung Series 5 Ultra 14", NP530U4B, around 2012 | Owner report |
| Installed OS | Windows 7 | Owner report |
| CPU | 64-bit Intel Core i3/i5, second generation Sandy Bridge | No |
| Display | 14", 1366×768, Intel HD 3000 graphics | No |
| Keyboard | Built-in, PS/2 | No |
| Network | Ethernet likely; adapter model unknown | No |
| Boot mode | BIOS supports CSM and UEFI; Windows 7 likely uses legacy mode | No |

In Windows 7, use **Win+R** → `msinfo32` for system type and CPU, `diskmgmt.msc` → Disk 0 properties → Volumes for partition style (MBR suggests BIOS, GPT suggests UEFI), and `devmgmt.msc` → Network adapters for the adapter.

## Firmware preparation {#bios}

1. Press **F2** during power-on.
2. **Advanced** → **Fast BIOS Mode** → **Disabled**; otherwise USB drives may not appear.
3. **F10** → **Yes** to save.

This setting can be restored later and does not alter Windows.

## Boot {#boot}

1. Write `OSel.bios.img` as described in [USB boot](../usb.md).
2. Power on and press **F10** for the boot menu.
3. Select the USB drive.

## Precautions {#risks}

{% note alert %}

Do not boot OSel in UEFI mode on this laptop. Some Samsung laptops from these years stopped booting after UEFI firmware settings were written. OSel does not write those settings, but BIOS mode is the safer test path.

{% endnote %}

OSel has no disk drivers and does not touch the Windows disk. If it hangs, hold power for 5–10 seconds, remove the USB drive and reboot.

## Restore settings {#restore}

After testing: **F2** → **Advanced** → **Fast BIOS Mode** → **Enabled** → **F10** → **Yes**. Restore the USB drive using [recovery instructions](../usb.md#recovery).
