---
description: "Decision 0018: a visible driver directory, smoltcp and outgoing TCP sockets for Go."
---

# 0018: Ethernet and outgoing TCP sockets

**Status:** accepted. **Date:** 2026-10-14.

## Context

Offline Tetris runs on a real computer. Network play needs HTTP and WebSocket; the public server is currently offline. Hardware drivers should be immediately visible in the repository.

## Decision

Place hardware drivers in `src/drivers/`, retaining the public `avganec`, `cereal` and `keyboard` names. Add PCI configuration access and Intel 82574L (`8086:10d3`) legacy DMA rings. Use no_std smoltcp 0.12 with IPv4, DHCP and TCP. A kernel thread services bounded batches every 10 ms, with adapter IRQs masked.

Expose outgoing TCP through the Linux ABI and existing descriptors, including Go's epoll modes. Bound sockets and buffers and release resources on the last open-description close. Test against the submodule's actual server handlers on an ephemeral local port; CI needs no external server.

## Alternatives

- A custom TCP stack would substantially expand the task and error surface.
- Virtio-net first would fit QEMU but not the planned Intel adapter; it can be added later.
- Supporting all Intel/Realtek/Wi-Fi adapters immediately is impractical without knowing the actual card and PHY. Support is limited to one testable ID.
- Depending on the public server would make tests unreliable while it is offline.

## Consequences

QEMU network play is independent of public-server availability. Other physical adapters need separate drivers. Polling bounds latency and throughput. IRQ/MSI, DNS, TLS, listening sockets and detached FIN/TIME_WAIT queues remain deferred. See [networking](../architecture/core/network.md) and [Ethernet](../architecture/drivers/ethernet.md) for architecture and restrictions.
