---
description: Ethernet, DHCP, TCP, Linux sockets and end-to-end network Tetris tests in OSel.
---

# Networking

**Audience:** developers of the network stack, sockets and drivers.

**Summary:** the Intel 82574L or Realtek driver passes Ethernet frames to smoltcp 0.12. The kernel obtains an IPv4 address through DHCP and exposes outgoing TCP connections through Linux socket calls. Go implements HTTP and WebSocket. QEMU tests use the actual Tetris server handlers from the submodule without a public server.

## How it works {#how}

After memory, scheduler and clock initialization, `net::init` discovers the PCI card, creates the interface and DHCP socket, and starts a kernel thread. Every 10 ms it processes at most 32 incoming frames, sends outgoing data, applies DHCP addresses and routes, updates socket readiness and wakes epoll. The stack never runs in an interrupt handler. Boot and offline programs continue without a supported card.

The runner attaches QEMU `e1000e` to user networking. QEMU provides DHCP and routing; the guest reaches its host at `10.0.2.2`. Hardware needs the supported adapter, an Ethernet cable and a DHCP server. Successful kernel boot does not demonstrate support for a different adapter.

Each TCP socket owns two 16 KiB buffers and an independent identifier. At most 64 sockets coexist. The smoltcp table is allocated at boot; dynamic buffers and an empty state tree are released on the last open-description close. Nonblocking `connect` returns `EINPROGRESS`; epoll and `SO_ERROR` report completion. Reads and writes return partial counts, `EAGAIN` or EOF. Blocking operations sleep without holding `NETWORK`.

## Sockets and epoll {#sockets}

IPv4/TCP implements `socket`, `connect`, `getsockname`, `getpeername`, `getsockopt`, `setsockopt`, `shutdown`, ordinary `read`/`write` and `writev`. Numbers, arguments and restrictions are in the [syscall reference](../../reference/syscalls.md). UDP, IPv6, `bind`, `listen`, `accept` and DNS are absent. Applications can use a numeric IPv4 address or a ramdisk `/etc/hosts` name.

Epoll keeps weak references to open descriptions: fd reuse never redirects an earlier registration. Level readiness, `EPOLLET`, `EPOLLONESHOT`, errors, hangups and `EPOLL_CTL_MOD` rearming are supported. Sockets, pipes and eventfd track new readiness transitions even when drained and refilled between scans. Unsupported interest bits return `EINVAL`; Linux epoll extensions are not all implemented.

## Code {#code}

- `src/drivers/pci.rs`, `src/drivers/ethernet.rs`, `src/drivers/e1000e.rs`, `src/drivers/rtl8168.rs`: PCI and [Ethernet driver](../drivers/ethernet.md).
- `src/net.rs`: `NETWORK`, service thread, DHCP, TCP and `SocketFile`.
- `osel-core/src/net.rs`: Linux IPv4 addresses, PCI addresses and BAR decoding, host tested.
- `src/syscall.rs`, `osel-core/src/syscall.rs`: Linux calls and user-buffer copying.
- `osel-core/src/file.rs`: socket identity, readiness and transition counters.
- `osel-core/src/poll.rs`, `src/poll.rs`: registrations and epoll waits.
- `runner/src/network_fixture.rs`, `tools/network-fixture/`: local test server.
- `tests/network.rs`, `tests/network_game.rs`, `user-go/netcheck/`: end-to-end checks.

## Invariants and locks {#locks}

`NETWORK` protects the interface, adapter and socket table. User pointers are validated and copied before taking it; user memory is never passed to DMA. No lock crosses a thread switch. The lock can nest `ALLOCATOR` for bounded buffers and `MEMORY` when DMA pages are released after failed initialization. `PCI_CONFIG` protects the configuration-port pair and is released before MMIO or printing.

## Pitfalls {#pitfalls}

- Intel 82574L (`8086:10d3`) and Realtek RTL8168 G/GU/H/M (`10ec:8168`) have drivers. QEMU Realtek C+ tests shared DMA, not the laptop PHY. The owner confirmed the Acer PCI ID; physical connectivity is unverified. Wi-Fi and USB Ethernet require separate drivers.
- The public server may be offline. Tests prove interoperability with the local repository server, not public-server availability.
- HTTP/WebSocket uses no TLS. Hostnames require `/etc/hosts`; DNS is absent.
- `SO_KEEPALIVE`, `TCP_KEEPIDLE` and `TCP_NODELAY` work. Separate keepalive intervals and probe counts return `ENOPROTOOPT`. Connection establishment is limited to 20 seconds; Go bounds a dial attempt to five seconds.
- The last descriptor close aborts TCP and immediately releases buffers. Detached FIN/TIME_WAIT queues are deferred. `shutdown` closes sending; independent read shutdown is unsupported.

## Tests {#tests}

`cargo test-core` checks addresses, BARs and epoll modes, including fd reuse, one-shot notifications and eventfd drain/refill between scans. `cargo test-runner` checks input scripts and local-server addresses.

`tests/network.rs` obtains a DHCP lease and checks refusal, a large stream with backpressure, HTTP, two WebSocket clients, matching fields after movement, ping/pong and repeated launch. `tests/network_game.rs` runs the actual client and external `clear`, receives both fields, sends a key, waits for pong and exits with `x`. The runner starts the local server on an ephemeral port and always terminates it after QEMU. Both firmware modes run the same checks; the common framework verifies heap and frame cleanup.

The same scenarios run through Realtek C+ in `tests/realtek_network.rs` and `tests/realtek_network_game.rs`; `tests/realtek_dma.rs` checks reinitialization and DMA teardown with page cleanup. The Acer PHY remains a hardware checkpoint.

## Decisions {#decisions}

- [0018: Ethernet and outgoing TCP sockets](../../decisions/0018-networking.md).

_Checked against code: 2026-10-14, stage-4-network-tetris branch._
