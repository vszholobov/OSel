---
description: Run network Tetris with a chosen server and inspect OSel Ethernet status.
---

# Network Tetris

**Audience:** users of OSel's networking stage.

**Summary:** offline `tetris` behaves as before. Network play requires a running server and a supported Ethernet card. This version tests Intel 82574L in QEMU; networking on the owner's computer remains unverified.

## Prepare a server {#server}

The public server is currently offline. Run the Tetris repository server on another computer with Go already installed:

```shell
cd apps/tetris/server
go run . -addr 0.0.0.0:8080
```

QEMU reaches its host server at `10.0.2.2:8080`. On hardware, use the server machine's IPv4 address in the same network. The server uses HTTP and WebSocket without TLS.

## Check the adapter {#adapter}

Enter `net` in the shell to see the adapter, link state, address and packet counters. DHCP provides the address. Hardware support is limited to Intel 82574L (`8086:10d3`) and an Ethernet cable. Wi-Fi, USB Ethernet and other models are unsupported.

Attach the adapter explicitly when launching a ready BIOS image:

```shell
qemu-system-x86_64 -drive format=raw,file=OSel.bios.img -serial stdio -no-reboot -netdev user,id=net0 -device e1000e,netdev=net0
```

The `cargo run` runner adds it automatically. Use an image built with the networking stage: release 0.1.0 does not include networking.

## Play {#play}

```text
tetris --network --server 10.0.2.2:8080
```

Menu keys: `c` creates, `r` refreshes, `w`/`s` select and Enter joins. The second player must join the same session. Direct creation/join is also available:

```text
tetris --network --server 10.0.2.2:8080 --create
tetris --network --server 10.0.2.2:8080 --session 123456789
```

Controls: `a`/`d` move, `s` moves down, `q`/`e` rotate. `x`, Escape or Ctrl+C return to the shell. Both fields fit an 80-column terminal.

## Connection failures {#errors}

An offline server produces an error and returns to the shell. Check the address, port 8080, running server and TCP reachability. No card in `net` means another driver is needed. A card without an address suggests a cable or DHCP issue. Use numeric IPv4 addresses because DNS is absent. Hostnames can be added to the ramdisk `/etc/hosts` before building.

Developers: [network tests](../developer/architecture/core/network.md#tests).
