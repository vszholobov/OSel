---
description: Acer Nitro UEFI boot, Secure Boot, BitLocker preparation and F12 boot menu for OSel.
---

# Acer Nitro

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
| Wired adapter | Realtek Gaming GBE Family Controller; `PCI\VEN_10EC&DEV_8168&SUBSYS_13661025&REV_25` | Owner provided Windows 10 hardware ID |
| Wi-Fi | Qualcomm Atheros QCA61x4A Wireless Network Adapter | Owner report; no OSel driver |

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

## Wired networking {#network}

The kernel includes a Realtek `10ec:8168` driver. It identifies G/GU/H/M MACs using the XID register; PCI `REV_25` alone does not identify the MAC. PHY startup on this Acer remains untested. Samsung is outside this hardware check.

Connect an Ethernet cable to a router providing DHCP, boot an image built from the networking PR, and enter `net`. Expect Realtek, `link=true` and an IPv4 address. If boot reports `unknown MAC XID` or `PHY ... timed out`, record the `NET` line containing XID and the error: PCI discovery succeeded, initialization did not. An empty address list with `link=true` means DHCP is pending or unavailable.

Once an address is assigned, run the server on another computer on the same network and use its IPv4 address; see [network Tetris](../network.md). `10.0.2.2` is specific to QEMU.

## Precautions {#risks}

OSel has no disk drivers and does not access the Windows disk or the laptop's NVMe storage. If this is a work laptop, begin with [Samsung NP530U4B](samsung-np530u4b.md), which needs fewer settings changes. On a hang, hold power for 5–10 seconds, remove the drive and reboot.

## Restore settings {#restore}

1. **F2** → **Boot** → **Secure Boot** → **Enabled**.
2. To remove an unneeded supervisor password: **Security** → **Set Supervisor Password**, enter the current password and leave the new one empty.
3. **F10** → **Yes**.

BitLocker may request the key again after restoring Secure Boot. See [recovery](../usb.md#recovery) to reuse the USB drive normally.

## Wi-Fi {#wifi}

Owner card: `PCI\VEN_168C&DEV_003E&SUBSYS_080711AD&REV_32`, QCA6174, LiteOn `11ad:0807`. Firmware hw3.0/API6 and the exact board entry are bundled. Only the ROM probe is implemented; WPA2 connection is absent. Acer needs `wifi probe` and `wifi regs`; see [report collection](../diagnostics.md#report).
