---
description: OSel commands for network, PCI, memory and initial Acer Wi-Fi diagnostics.
---

# Diagnostics

**Audience:** users checking networking or reporting a failure.

**Summary:** `diag` collects a general report. After DHCP, `ping gateway` checks the router and `ping IP` checks an IPv4 destination. Acer supports `wifi probe` and its saved `wifi regs` report; Wi-Fi connection is not implemented yet.

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
| `wifi regs` | saved stages and values from the last probe, without rereading registers |
| `kbd` | PS/2 initialization, IRQ and input counters |

`ping` sends at most one request per second, waits up to 1 second and stops on Ctrl+C. Timeout means no ICMP reply: a Tetris server may work while blocking ICMP. `ENETUNREACH` means no adapter, link or DHCP address. The gateway must come from DHCP; `10.0.2.2` is only for QEMU guests. Multicast, loopback and broadcast targets are rejected.

`nslookup` runs in the shell. Go programs still need numeric IPv4 or `/etc/hosts`: Linux UDP sockets for their resolver are not implemented. A returned address can be passed to the game manually.

## Send a report {#report}

1. After boot, run `clear`, then `diag`. Save COM1 output or photograph the display including version and commit.
2. For [Acer Wi-Fi](hardware/acer-nitro.md#wifi), run `wifi probe`, then `clear` and `wifi regs`; save that display too. The error and stage help even when probing fails.
3. For Ethernet after DHCP, run `ping gateway 4`, then `ping SERVER_IP 4`. Check `tcpcheck SERVER_IP 8080`. For game failures include the error text and server address.

Reports contain no Wi-Fi passwords or keys. All shell output is also sent to COM1. If the laptop freezes during `wifi probe`, preserve the last line: stages are shown on the display and sent to COM1 while executing. Probing does not run automatically at boot, so the shell remains available after reboot.

## Verified scope {#limits}

QEMU verifies commands, missing QCA6174, actual ICMP and memory cleanup. QEMU does not emulate QCA6174. BMI responses, firmware loading, scanning, WPA2, DHCP and Wi-Fi gameplay require separate Acer verification. The current image contains initial bring-up diagnostics, not working Wi-Fi.
