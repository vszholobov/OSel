---
description: OSel commands for network, PCI, memory and initial Acer Wi-Fi diagnostics.
---

# Diagnostics

**Audience:** users checking networking or reporting a failure.

**Summary:** `diag` collects a general report. After DHCP, `ping gateway` checks the router and `ping IP` checks an IPv4 destination. Acer supports `wifi probe` and its saved `wifi regs` report; WPA2 connectivity and a network game are confirmed on Acer; sustained stability remains under test.

## Commands {#commands}

| Command | Information or action |
|---|---|
| `diag` | version, commit, uptime, PIT, memory, keyboard, network PCI cards, Ethernet, DHCP, routes, DNS, TCP and Wi-Fi status |
| `pci` | PCI ID, subsystem, revision, class, BAR0 and PCI command |
| `net` | adapter, link, addresses, sockets and received/sent/dropped frames |
| `net ip` | MAC, address/mask, DHCP readiness and configuration count |
| `net route` | connected subnet and default gateway |
| `net dns` | DHCP-provided DNS servers |
| `net tcp` | connections, endpoints, state, queues and errors |
| `net dhcp` | restarts DHCP discovery; check `net ip` a few seconds later |
| `ping gateway [count]` | ICMP Echo to the DHCP gateway |
| `ping IP [count]` | ICMP Echo, latency and loss; default 4 packets, allowed 1–10 |
| `tcpcheck IP port` | checks a TCP port within 3 seconds; distinguishes refusal and timeout |
| `nslookup name` | DNS IPv4 addresses, at most 5 seconds |
| `wifi status` | QCA6174 PCI card, last probe stage and error |
| `wifi fw` | bundled firmware version, API/WMI/HTT, OTP and exact board match |
| `wifi probe` | checks the Acer card up to a BMI ROM response; does not start firmware or connect |
| `wifi load` | repeats the ROM check, uploads calibration/firmware and waits for HTC/WMI ready; stops the target after the check, without connecting |
| `wifi scan` | loads firmware and merges three passive passes over 2.4 GHz and selected 5 GHz channels; lists up to 20 APs with SSID, BSSID, frequency, signal and WPA2 compatibility, then stops the target |
| `wifi results` | saved results of the last successful scan without accessing hardware |
| `wifi connect N` | connects to WPA2 AP number N from wifi results; separate hidden password input, followed by DHCP |
| `wifi disconnect` | stops Wi-Fi, clears network configuration and aborts connections |
| `wifi regs` | last 22 saved stages and values from the probe, without rereading registers |
| `kbd` | PS/2 initialization, IRQ and input counters |

`ping` sends at most one request per second, waits up to 1 second and stops on Ctrl+C. Timeout means no ICMP reply: a Tetris server may work while blocking ICMP. `ENETUNREACH` means no adapter, link or DHCP address. The gateway must come from DHCP; `10.0.2.2` is only for QEMU guests. Multicast, loopback and broadcast targets are rejected.

`nslookup` runs in the shell. Go programs still need numeric IPv4 or `/etc/hosts`: Linux UDP sockets for their resolver are not implemented. A returned address can be passed to the game manually.

## Send a report {#report}

1. After boot, run `clear`, then `diag`. Save COM1 output or photograph the display including version and commit.
2. For [Acer Wi-Fi](hardware/acer-nitro.md#wifi), run `wifi probe`, then `clear` and `wifi regs`; save that display too. The error and stage help even when probing fails.
3. For Ethernet after DHCP, run `ping gateway 4`, then `ping SERVER_IP 4`. Check `tcpcheck SERVER_IP 8080`. For game failures include the error text and server address.

`wifi load` has a 120-second deadline after the ROM check and supports Ctrl+C. Stages and uploaded byte counts appear on screen. `firmware check passed; target stopped` confirms firmware startup through HTC/WMI; then run `wifi scan`, select an AP with `wifi connect N` and check `net ip` and `ping gateway`. The owner confirmed Acer connectivity, ping and a network Tetris game. Save `wifi regs` on failure.

Reports contain no Wi-Fi passwords or keys. All shell output is also sent to COM1. If the laptop freezes during `wifi probe`, preserve the last line: stages are shown on the display and sent to COM1 while executing. Probing does not run automatically at boot, so the shell remains available after reboot.

## Connect Wi-Fi {#connect}

In a new image run `wifi scan`, then `wifi connect N`, selecting a WPA2 network number. Enter the password on the laptop without echo; it is not needed in a report. Requirements: visible SSID, WPA2-Personal/CCMP, 2.4 GHz and channels 1–11. WPA3-only, Enterprise, mandatory PMF and hidden SSIDs are unsupported. Power is capped at 10 dBm with board calibration retained; test near the router initially.

Discovery covers more channels than association: channels 1–13 and selected 5 GHz channels within board limits are scanned, while connection accepts only 1–11. DFS and 6 GHz are excluded. A nearby phone hotspot may therefore be absent; the phone model does not establish its configured band.

Each rejected network shows `Cannot connect:` with the exact reason. `group cipher requires CCMP/AES` means the AP's group encryption is unsupported, even if its password is correct and unicast offers AES. `no WPA2 RSN` means no suitable WPA2 description; WPA1/WEP are not implemented. PSK and mandatory PMF are checked separately. A compatible router configuration requires WPA2-Personal and AES/CCMP for both group and pairwise encryption; mandatory PMF is unsupported. The old `UNSUPPORTED` output alone does not establish Rostel-6's exact security mode.

For empty or intermittent results run `clear`, `wifi regs`: preserve `passive scan channel visits`, `management frames`, `bad WMI frames`, `bad beacons`, `networks` and the commit from `diag`. Three passes merge within one command; `No saved networks` does not establish that APs are absent. The deeper receive queue and repeated passes still need Acer validation.

After `WiFi DHCP ready`, run `net ip`, `ping gateway 4`, `tcpcheck SERVER_IP 8080`, then [Tetris](network.md). On failure run `clear`, `wifi regs`, `wifi status` and `diag`; include the last stage, errno and commit. A DHCP timeout leaves the worker active; retry `net dhcp` after fixing DHCP. Use `wifi disconnect` before reconnecting.

For a connection ETIMEDOUT preserve `wifi regs`. `STA HTT TX completion wait` means no transmission completion reached the driver; `STA authentication response wait` means TX completed but no matching AP response was accepted. Both precede WPA2 password verification. Reports include TX ID, completion status and receive counters, never the password.

If the TX wait returns EIO after `HTT management TX DROP`, firmware reported discarding the frame; `NOACK` means completion without acknowledgement. This differs from a missing TX completion with ETIMEDOUT. Preserve the complete `wifi regs` report and commit from `diag`; the first open authentication does not yet verify the WPA2 password. DROP alone does not establish its cause or whether the frame was transmitted over the air.

## Verified scope {#limits}

QEMU verifies commands, missing QCA6174, actual ICMP, the WPA2 state machine, DMA ownership, adapter switching and link-loss cleanup. QEMU does not emulate QCA6174. Acer photos confirm HTT 3.87 and an 18-AP result with Galaxy S23. The owner confirmed connectivity, ping and Wi-Fi gameplay with a Mac; sustained stability still requires verification; see the current [driver status](../developer/architecture/drivers/wifi.md#abi-audit).

If Wi-Fi disconnects after a successful connection, save `wifi regs` and `wifi status`. `STA WMI roam reason=00000002` means missed beacons: the driver checks AP liveness for up to three seconds and closes the link only without confirmation. `STA AP liveness probe` identifies an attempt; `STA AP liveness confirmed` reports its ACK. After 30 seconds without confirmed activity, an idle check runs. `STA AP liveness timeout` means the check could not confirm the link. Explicit kickout/deauth and peer loss still close it immediately. The new monitor still needs sustained Acer validation; automatic password entry and reconnection are not implemented.
