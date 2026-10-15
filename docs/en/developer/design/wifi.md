---
description: Acer QCA6174 WPA2 bring-up plan from a hardware probe to network Tetris.
---

# Acer Wi-Fi

**Audience:** developers of the next network stage.

**Summary:** the target is QCA6174 PCI `168c:003e`, subsystem `11ad:0807`, rev `32`. The first reviewable result is a diagnostic image with a ROM probe. Firmware loading, scanning, WPA2 association and the packet path are implemented; hardware connectivity validation remains pending.

## Stages {#stages}

1. **PCI and ROM.** Confirmed on Acer with images `a53f703` and `8a7c625`: D0/wake/reset, CE7, early configuration and BMI Target Info `05030000`, type `8`, error=None.
2. **Firmware.** BMI host-interest, EFUSE-selected PLL, exact board/OTP and SGMT upload, BMI_DONE and HTC/WMI ready validation are implemented. `wifi load` checks startup and stops the target. Photos confirm upload and WMI ready; HTT 3.87 and an 18-AP list with Galaxy S23 are confirmed, but repeated scans are unreliable and connectivity still requires hardware results, see the [ABI audit](../architecture/drivers/wifi.md#abi-audit).
3. **Networks.** Passive `wifi scan/results`, WMI TLV, 2.4 GHz channels, scan events, BSSID/SSID/channel/RSSI. Checked 802.11/RSN parsing. Initial target: visible SSID and WPA2-Personal CCMP, without WPA3/Enterprise/roaming. Region and permitted channels cannot be guessed from PCI identity.
4. **Connection.** Authentication/association, EAPOL 4-way handshake, replay/MIC and PTK/GTK installation. Password input must be separate without echo, repository storage or diagnostic disclosure. Nonces require sufficient randomness, not only PIT or MAC.
5. **Data.** HTT RX/TX and frame translation into the existing smoltcp Ethernet interface. After association: DHCP, route, DNS, `ping gateway` and Tetris. Link loss invalidates configuration/connections; adapter switching must not retain stale SocketHandle values.
6. **Completion.** Repeated connect/disconnect and wrong-password cases leak no resources; UEFI/BIOS and existing Ethernet remain green. Acer verifies DHCP, ping and two players with a local server. The public server is tested after the owner starts it.

## Hardware check {#hardware}

Commands and report collection: [diagnostics](../../user/diagnostics.md#report). HTT 3.87 and an 18-AP list with Galaxy S23 are confirmed on Acer; Wi-Fi completion requires reliable discovery, DHCP/ping and gameplay. Current software checks are described in the [ABI audit](../architecture/drivers/wifi.md#abi-audit); bundled firmware and green CI do not complete connectivity.

Code for stages 2–5 and its limits are documented in the [driver architecture](../architecture/drivers/wifi.md#station). `wifi connect N` reads the password on the laptop; checked RDRAND supplies nonces. Stage 6 hardware checks remain open.
