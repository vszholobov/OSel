---
description: Acer QCA6174 WPA2 bring-up plan from a hardware probe to network Tetris.
---

# Acer Wi-Fi

**Audience:** developers of the next network stage.

**Summary:** the target is QCA6174 PCI `168c:003e`, subsystem `11ad:0807`, rev `32`. The first reviewable result is a diagnostic image with a ROM probe. Working Wi-Fi still requires firmware loading, scanning, WPA2 association and a packet data path.

## Stages {#stages}

1. **PCI and ROM.** Confirmed on Acer with images `a53f703` and `8a7c625`: D0/wake/reset, CE7, early configuration and BMI Target Info `05030000`, type `8`, error=None.
2. **Firmware.** BMI host-interest, EFUSE-selected PLL, exact board/OTP and SGMT upload, BMI_DONE and HTC ready validation are implemented. `wifi load` checks startup and stops the target. Hardware validation is pending; next is HTC service connection and WMI service-ready.
3. **Networks.** WMI TLV, 2.4 GHz channels, scan events, BSSID/SSID/channel/RSSI. Checked 802.11/RSN parsing. Initial target: visible SSID and WPA2-Personal CCMP, without WPA3/Enterprise/roaming. Region and permitted channels cannot be guessed from PCI identity.
4. **Connection.** Authentication/association, EAPOL 4-way handshake, replay/MIC and PTK/GTK installation. Password input must be separate without echo, repository storage or diagnostic disclosure. Nonces require sufficient randomness, not only PIT or MAC.
5. **Data.** HTT RX/TX and frame translation into the existing smoltcp Ethernet interface. After association: DHCP, route, DNS, `ping gateway` and Tetris. Link loss invalidates configuration/connections; adapter switching must not retain stale SocketHandle values.
6. **Completion.** Repeated connect/disconnect and wrong-password cases leak no resources; UEFI/BIOS and existing Ethernet remain green. Acer verifies DHCP, ping and two players with a local server. The public server is tested after the owner starts it.

## Hardware check {#hardware}

Commands and report collection: [diagnostics](../../user/diagnostics.md#report). Working Wi-Fi is not claimed before ROM validation. The PR remains a hardware-stage draft: bundled firmware and green CI do not complete connectivity.
