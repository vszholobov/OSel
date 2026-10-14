---
description: Acer QCA6174 WPA2 bring-up plan from a hardware probe to network Tetris.
---

# Acer Wi-Fi

**Audience:** developers of the next network stage.

**Summary:** the target is QCA6174 PCI `168c:003e`, subsystem `11ad:0807`, rev `32`. The first reviewable result is a diagnostic image with a ROM probe. Working Wi-Fi still requires firmware loading, scanning, WPA2 association and a packet data path.

## Stages {#stages}

1. **PCI and ROM.** Unmodified bundled files and exact board, bounded D0/wake/reset/CE/BMI and saved report. PCI/D0/wake/reset are confirmed on Acer; the first BMI attempt timed out at CE0. ROM early configuration over CE7 and CPU signalling have been added; another Acer `wifi probe`/`wifi regs` must confirm DMA and target version. QEMU cannot test this path.
2. **Firmware.** BMI host-interest, PLL, OTP/calibration, image loading, all CE configuration and BMI_DONE. HTC ready and WMI service-ready must confirm startup; timeouts preserve the stage and release resources. Firmware running is not set before those events.
3. **Networks.** WMI TLV, 2.4 GHz channels, scan events, BSSID/SSID/channel/RSSI. Checked 802.11/RSN parsing. Initial target: visible SSID and WPA2-Personal CCMP, without WPA3/Enterprise/roaming. Region and permitted channels cannot be guessed from PCI identity.
4. **Connection.** Authentication/association, EAPOL 4-way handshake, replay/MIC and PTK/GTK installation. Password input must be separate without echo, repository storage or diagnostic disclosure. Nonces require sufficient randomness, not only PIT or MAC.
5. **Data.** HTT RX/TX and frame translation into the existing smoltcp Ethernet interface. After association: DHCP, route, DNS, `ping gateway` and Tetris. Link loss invalidates configuration/connections; adapter switching must not retain stale SocketHandle values.
6. **Completion.** Repeated connect/disconnect and wrong-password cases leak no resources; UEFI/BIOS and existing Ethernet remain green. Acer verifies DHCP, ping and two players with a local server. The public server is tested after the owner starts it.

## Hardware check {#hardware}

Commands and report collection: [diagnostics](../../user/diagnostics.md#report). Working Wi-Fi is not claimed before ROM validation. The PR remains a hardware-stage draft: bundled firmware and green CI do not complete connectivity.
