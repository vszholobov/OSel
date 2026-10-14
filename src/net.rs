//! Ethernet + DHCP + IPv4/TCP. A kernel thread services the stack every timer tick.
//! Socket descriptions own their handles; dropping the last reference releases buffers.
use crate::{
    drivers::{ethernet::Ethernet, pci},
    poll, scheduler,
    sync::IrqSpinlock,
    time,
};
use alloc::{collections::BTreeMap, sync::Arc, vec, vec::Vec};
use osel_core::{
    errno::{Errno, KResult},
    file::{FileLike, Ready},
    net::Endpoint,
    vfs::{FileType, Stat},
};
use smoltcp::{
    iface::{Config, Interface, SocketHandle, SocketSet},
    socket::{dhcpv4, dns, icmp, tcp},
    time::{Duration, Instant},
    wire::{EthernetAddress, IpAddress, IpCidr, IpEndpoint, Ipv4Address},
};
const MAX_SOCKETS: usize = 64;
const CAPACITY: usize = 16 * 1024;
static NETWORK: IrqSpinlock<Option<Network>> = IrqSpinlock::new("NETWORK", None);
struct State {
    handle: SocketHandle,
    connecting: bool,
    connected: bool,
    error: Option<Errno>,
    flags: u32,
    epochs: [u64; 2],
    started_ns: u64,
}
struct Network {
    device: Ethernet,
    interface: Interface,
    sockets: SocketSet<'static>,
    dhcp: SocketHandle,
    states: BTreeMap<usize, State>,
    next_id: usize,
    port: u16,
    configured: bool,
    gateway: Option<Ipv4Address>,
    dns: Vec<Ipv4Address>,
    leases: u64,
    next_ping: u16,
    pings: usize,
    lookups: usize,
}
fn now() -> Instant {
    Instant::from_millis((time::monotonic_ns() / 1_000_000) as i64)
}
impl Network {
    fn service(&mut self) {
        // Bound the work even under an incoming packet flood.
        for _ in 0..32 {
            if self
                .interface
                .poll_ingress_single(now(), &mut self.device, &mut self.sockets)
                == smoltcp::iface::PollIngressSingleResult::None
            {
                break;
            }
        }
        self.interface
            .poll_egress(now(), &mut self.device, &mut self.sockets);
        match self.sockets.get_mut::<dhcpv4::Socket>(self.dhcp).poll() {
            Some(dhcpv4::Event::Configured(config)) => {
                self.gateway = config.router;
                self.dns.clear();
                self.dns.extend(config.dns_servers.iter().copied());
                self.leases += 1;
                self.interface.update_ip_addrs(|addrs| {
                    addrs.clear();
                    addrs.push(IpCidr::Ipv4(config.address)).unwrap();
                });
                self.interface.routes_mut().remove_default_ipv4_route();
                if let Some(router) = config.router {
                    let _ = self.interface.routes_mut().add_default_ipv4_route(router);
                }
                self.configured = true;
            }
            Some(dhcpv4::Event::Deconfigured) => {
                self.gateway = None;
                self.dns.clear();
                self.configured = false;
                self.interface.update_ip_addrs(|addrs| addrs.clear());
                self.interface.routes_mut().remove_default_ipv4_route();
            }
            None => {}
        }
        for state in self.states.values_mut() {
            update(state, self.sockets.get_mut::<tcp::Socket>(state.handle));
        }
    }
}
fn update(state: &mut State, socket: &mut tcp::Socket) {
    if state.connecting {
        if socket.state() == tcp::State::Established {
            state.connecting = false;
            state.connected = true;
            socket.set_timeout(None);
        } else if socket.state() == tcp::State::Closed {
            state.connecting = false;
            state.error = Some(
                if time::monotonic_ns().saturating_sub(state.started_ns) >= 20_000_000_000 {
                    Errno::ETIMEDOUT
                } else {
                    Errno::ECONNREFUSED
                },
            );
        }
    }
    let eof = state.connected && !socket.may_recv();
    let mut flags = 0;
    if socket.can_recv() || eof {
        flags |= 1;
    }
    if socket.can_send() {
        flags |= 4;
    }
    if state.error.is_some() {
        flags |= 8 | 4 | 1;
    }
    if eof {
        flags |= 0x2000;
    }
    if state.connected && !socket.is_open() {
        flags |= 0x10;
    }
    for (i, mask) in [1 | 8 | 0x10 | 0x2000, 4 | 8 | 0x10]
        .into_iter()
        .enumerate()
    {
        if flags & mask != 0 && flags & mask != state.flags & mask {
            state.epochs[i] = state.epochs[i].wrapping_add(1);
        }
    }
    state.flags = flags;
}
pub fn init() {
    let devices = pci::scan();
    for d in &devices {
        crate::serial_println!(
            "PCI {:02x}:{:02x}.{} {:04x}:{:04x} class {:02x}:{:02x}",
            d.bus,
            d.slot,
            d.function,
            d.vendor,
            d.device,
            d.class,
            d.subclass
        );
    }
    let Some(mut device) = devices.into_iter().find_map(Ethernet::new) else {
        crate::serial_println!(
            "NET: no supported Ethernet card (Intel 82574L or Realtek RTL8168 G/GU/H/M)"
        );
        return;
    };
    let mac = device.mac();
    let mut config = Config::new(EthernetAddress(mac).into());
    config.random_seed = time::realtime_ns()
        ^ u64::from_le_bytes([mac[0], mac[1], mac[2], mac[3], mac[4], mac[5], 0, 0]);
    let interface = Interface::new(config, &mut device, now());
    let storage = (0..MAX_SOCKETS + 9)
        .map(|_| smoltcp::iface::SocketStorage::EMPTY)
        .collect::<Vec<_>>();
    let mut sockets = SocketSet::new(storage);
    let dhcp = sockets.add(dhcpv4::Socket::new());
    crate::serial_println!("NET: {}, MAC {:02x?}, DHCP", device.name(), mac);
    *NETWORK.lock() = Some(Network {
        device,
        interface,
        sockets,
        dhcp,
        states: BTreeMap::new(),
        next_id: 1,
        port: 49152,
        configured: false,
        gateway: None,
        dns: Vec::with_capacity(3),
        leases: 0,
        next_ping: 1,
        pings: 0,
        lookups: 0,
    });
    scheduler::spawn_kernel(worker).expect("network worker stack");
}
fn worker() {
    loop {
        if let Some(net) = NETWORK.lock().as_mut() {
            net.service();
        }
        poll::notify();
        let _ = time::sleep_until(time::monotonic_ns().saturating_add(10_000_000));
    }
}
pub fn wait_ready() -> KResult<()> {
    let deadline = time::monotonic_ns().saturating_add(10_000_000_000);
    loop {
        let ready = NETWORK
            .lock()
            .as_ref()
            .map(|net| net.configured)
            .ok_or(Errno::ENETUNREACH)?;
        if ready {
            return Ok(());
        }
        if time::monotonic_ns() >= deadline {
            return Err(Errno::ENETUNREACH);
        }
        time::sleep_until(time::monotonic_ns() + 10_000_000)?;
    }
}
pub fn status(out: &mut impl core::fmt::Write) {
    // Snapshot while locked; framebuffer output may be slow and take unrelated locks.
    let snapshot = NETWORK.lock().as_ref().map(|net| {
        (
            net.device.name(),
            net.device.link(),
            net.interface.ip_addrs().to_vec(),
            net.states.len(),
            net.device.counters(),
        )
    });
    if let Some((name, link, addresses, sockets, (rx, tx, dropped))) = snapshot {
        let _ = writeln!(
            out,
            "Ethernet {name}: link={link}, addresses={addresses:?}, sockets={sockets}, rx={rx}, tx={tx}, dropped={dropped}"
        );
    } else {
        let _ = writeln!(
            out,
            "No initialized Ethernet card: Intel 8086:10d3 or Realtek 10ec:8168 (G/GU/H/M); see NET boot diagnostics"
        );
    }
}

/// Diagnostics snapshot before writing to the console: never hold NETWORK over output.
pub fn diagnostics(section: &str, out: &mut impl core::fmt::Write) -> KResult<()> {
    if !matches!(section, "ip" | "route" | "dns" | "tcp" | "dhcp") {
        return Err(Errno::EINVAL);
    }
    let snapshot = {
        let mut guard = NETWORK.lock();
        let net = guard.as_mut().ok_or(Errno::ENETUNREACH)?;
        if section == "dhcp" {
            net.sockets.get_mut::<dhcpv4::Socket>(net.dhcp).reset();
        }
        let tcp: Vec<_> = if section == "tcp" {
            net.states
                .iter()
                .map(|(id, state)| {
                    let socket = net.sockets.get::<tcp::Socket>(state.handle);
                    (
                        *id,
                        socket.state(),
                        socket.local_endpoint(),
                        socket.remote_endpoint(),
                        socket.recv_queue(),
                        socket.send_queue(),
                        state.error,
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
        (
            net.device.mac(),
            net.configured,
            net.interface.ip_addrs().to_vec(),
            net.gateway,
            net.dns.clone(),
            net.leases,
            tcp,
        )
    };
    let (mac, ready, addresses, gateway, dns, leases, tcp) = snapshot;
    match section {
        "ip" => {
            let _ = writeln!(
                out,
                "MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}\nDHCP configured={ready} leases={leases}",
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
            );
            for address in addresses {
                let _ = writeln!(out, "IPv4 {address}");
            }
        }
        "route" => {
            for address in addresses {
                let _ = writeln!(out, "connected {address}");
            }
            if let Some(gateway) = gateway {
                let _ = writeln!(out, "default via {gateway}");
            } else {
                let _ = writeln!(out, "No default route");
            }
        }
        "dns" => {
            if dns.is_empty() {
                let _ = writeln!(out, "No DNS servers from DHCP");
            }
            for server in dns {
                let _ = writeln!(out, "DNS {server}");
            }
        }
        "tcp" => {
            let _ = writeln!(out, "TCP sockets: {}", tcp.len());
            for (id, state, local, remote, rx, tx, error) in tcp {
                let _ = writeln!(
                    out,
                    "#{id} {state:?} {local:?} -> {remote:?}\n  queued rx={rx} tx={tx} error={error:?}"
                );
            }
        }
        "dhcp" => {
            let _ = writeln!(
                out,
                "DHCP discovery requested; inspect net ip in a few seconds"
            );
        }
        _ => unreachable!(),
    }
    Ok(())
}

struct PingSocket(SocketHandle);
impl Drop for PingSocket {
    fn drop(&mut self) {
        if let Some(net) = NETWORK.lock().as_mut() {
            net.sockets.remove(self.0);
            net.pings -= 1;
        }
    }
}

struct DnsSocket(SocketHandle);
impl Drop for DnsSocket {
    fn drop(&mut self) {
        if let Some(net) = NETWORK.lock().as_mut() {
            net.sockets.remove(self.0);
            net.lookups -= 1;
        }
    }
}

/// Resolve A records through the actual DHCP-provided DNS servers, bounded to 5 seconds.
pub fn lookup(name: &str) -> KResult<Vec<Ipv4Address>> {
    let (socket, query) = {
        let mut guard = NETWORK.lock();
        let net = guard.as_mut().ok_or(Errno::ENETUNREACH)?;
        if !net.configured || net.dns.is_empty() {
            return Err(Errno::ENETUNREACH);
        }
        if net.lookups >= 4 {
            return Err(Errno::EAGAIN);
        }
        let servers: Vec<_> = net.dns.iter().map(|ip| IpAddress::Ipv4(*ip)).collect();
        let mut socket = dns::Socket::new(&servers, vec![None]);
        let query = socket
            .start_query(
                net.interface.context(),
                name,
                smoltcp::wire::DnsQueryType::A,
            )
            .map_err(|_| Errno::EINVAL)?;
        let handle = net.sockets.add(socket);
        net.lookups += 1;
        (DnsSocket(handle), query)
    };
    let deadline = time::monotonic_ns().saturating_add(5_000_000_000);
    loop {
        let result = {
            let mut guard = NETWORK.lock();
            let net = guard.as_mut().ok_or(Errno::ENETUNREACH)?;
            net.sockets
                .get_mut::<dns::Socket>(socket.0)
                .get_query_result(query)
        };
        match result {
            Ok(addresses) => {
                return Ok(addresses
                    .into_iter()
                    .map(|ip| match ip {
                        IpAddress::Ipv4(ip) => ip,
                    })
                    .collect());
            }
            Err(dns::GetQueryResultError::Failed) => return Err(Errno::ENOENT),
            Err(dns::GetQueryResultError::Pending) => {}
        }
        if time::monotonic_ns() >= deadline {
            return Err(Errno::ETIMEDOUT);
        }
        time::sleep_until(time::monotonic_ns().saturating_add(10_000_000))?;
    }
}

/// Probe an actual TCP service, with no HTTP/game data and a three-second deadline.
pub fn tcp_check(address: &str, port: u16) -> KResult<u64> {
    let address = address.parse::<Ipv4Address>().map_err(|_| Errno::EINVAL)?;
    if port == 0
        || address.is_unspecified()
        || address.is_multicast()
        || address.is_broadcast()
        || address.is_loopback()
    {
        return Err(Errno::EINVAL);
    }
    {
        let guard = NETWORK.lock();
        let net = guard.as_ref().ok_or(Errno::ENETUNREACH)?;
        if !net.configured || !net.device.link() {
            return Err(Errno::ENETUNREACH);
        }
        if net.interface.ip_addrs().iter().any(|cidr| match cidr {
            IpCidr::Ipv4(cidr) => cidr.broadcast() == Some(address),
        }) {
            return Err(Errno::EINVAL);
        }
    }
    let file = socket()?;
    let id = file.socket_id().ok_or(Errno::ENOTSOCK)?;
    let start = time::monotonic_ns();
    match connect(
        id,
        Endpoint {
            ip: address.octets(),
            port,
        },
        true,
    ) {
        Err(Errno::EINPROGRESS) | Ok(()) => {}
        Err(error) => return Err(error),
    }
    loop {
        let connected = {
            let guard = NETWORK.lock();
            let state = guard
                .as_ref()
                .ok_or(Errno::ENETUNREACH)?
                .states
                .get(&id)
                .ok_or(Errno::ENOTSOCK)?;
            if let Some(error) = state.error {
                return Err(error);
            }
            state.connected
        };
        if connected {
            let elapsed_us = time::monotonic_ns().saturating_sub(start) / 1000;
            shutdown(id, 1)?;
            // Let the worker send FIN before removing the socket and its buffers.
            time::sleep_until(time::monotonic_ns().saturating_add(100_000_000))?;
            return Ok(elapsed_us);
        }
        if time::monotonic_ns().saturating_sub(start) >= 3_000_000_000 {
            return Err(Errno::ETIMEDOUT);
        }
        time::sleep_until(time::monotonic_ns().saturating_add(10_000_000))?;
    }
}

/// Bounded synchronous ping. The worker owns polling; waits release NETWORK and IRQs.
/// Return 1 for packet loss and an errno for invalid input or unavailable networking.
pub fn ping(target: &str, count: u16, out: &mut impl core::fmt::Write) -> KResult<u8> {
    ping_with_cancel(target, count, out, || false)
}
pub fn ping_with_cancel(
    target: &str,
    count: u16,
    out: &mut impl core::fmt::Write,
    mut cancelled: impl FnMut() -> bool,
) -> KResult<u8> {
    if !(1..=10).contains(&count) {
        return Err(Errno::EINVAL);
    }
    let nonce = time::monotonic_ns();
    let (address, ident, handle) = {
        let mut guard = NETWORK.lock();
        let net = guard.as_mut().ok_or(Errno::ENETUNREACH)?;
        let address = if target == "gateway" {
            net.gateway.ok_or(Errno::ENETUNREACH)?
        } else {
            target.parse::<Ipv4Address>().map_err(|_| Errno::EINVAL)?
        };
        if address.is_multicast()
            || address.is_broadcast()
            || address.is_unspecified()
            || address.is_loopback()
        {
            return Err(Errno::EINVAL);
        }
        if net.interface.ip_addrs().iter().any(|cidr| match cidr {
            IpCidr::Ipv4(cidr) => cidr.broadcast() == Some(address),
        }) {
            return Err(Errno::EINVAL);
        }
        if !net.configured || !net.device.link() {
            return Err(Errno::ENETUNREACH);
        }
        if net.pings == 4 {
            return Err(Errno::EAGAIN);
        }
        let ident = net.next_ping;
        net.next_ping = net.next_ping.wrapping_add(1);
        let buffer =
            || icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY; 4], vec![0; 2048]);
        let mut socket = icmp::Socket::new(buffer(), buffer());
        socket
            .bind(icmp::Endpoint::Ident(ident))
            .map_err(|_| Errno::EIO)?;
        let handle = net.sockets.add(socket);
        net.pings += 1;
        (address, ident, PingSocket(handle))
    };
    let _ = writeln!(
        out,
        "PING {address}: 32 data bytes, timeout 1000 ms (Ctrl+C stops)"
    );
    let mut sent = 0u16;
    let mut received = 0u16;
    let mut total_us = 0u64;
    let mut min_us = u64::MAX;
    let mut max_us = 0;
    'probes: for seq in 1..=count {
        if cancelled() {
            break;
        }
        let start = time::monotonic_ns();
        let bytes = osel_core::icmp::request(ident, seq, nonce);
        {
            let mut guard = NETWORK.lock();
            let net = guard.as_mut().ok_or(Errno::ENETUNREACH)?;
            net.sockets
                .get_mut::<icmp::Socket>(handle.0)
                .send_slice(&bytes, address.into())
                .map_err(|_| Errno::EAGAIN)?;
        }
        sent += 1;
        loop {
            if cancelled() {
                break 'probes;
            }
            let matched = {
                let mut guard = NETWORK.lock();
                let net = guard.as_mut().ok_or(Errno::ENETUNREACH)?;
                let socket = net.sockets.get_mut::<icmp::Socket>(handle.0);
                let mut matched = false;
                // Drain unrelated/old replies, bounded by the receive metadata capacity.
                for _ in 0..4 {
                    match socket.recv() {
                        Ok((bytes, source))
                            if source == IpAddress::Ipv4(address)
                                && osel_core::icmp::reply_matches(bytes, ident, seq, nonce) =>
                        {
                            matched = true;
                            break;
                        }
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
                matched
            };
            if matched {
                let us = time::monotonic_ns().saturating_sub(start) / 1000;
                received += 1;
                total_us += us;
                min_us = min_us.min(us);
                max_us = max_us.max(us);
                let _ = writeln!(
                    out,
                    "40 bytes from {address}: seq={seq} time={}.{:03} ms",
                    us / 1000,
                    us % 1000
                );
                break;
            }
            if time::monotonic_ns().saturating_sub(start) >= 1_000_000_000 {
                let _ = writeln!(out, "timeout from {address}: seq={seq}");
                break;
            }
            time::sleep_until(time::monotonic_ns().saturating_add(10_000_000))?;
        }
        // At most one request per second, including when the destination replies quickly.
        if seq < count {
            while time::monotonic_ns().saturating_sub(start) < 1_000_000_000 {
                if cancelled() {
                    break 'probes;
                }
                time::sleep_until(time::monotonic_ns().saturating_add(10_000_000))?;
            }
        }
    }
    let loss = if sent == 0 {
        0
    } else {
        (sent - received) * 100 / sent
    };
    let _ = writeln!(out, "{sent} sent, {received} received, {loss}% loss");
    if received > 0 {
        let avg = total_us / u64::from(received);
        let _ = writeln!(
            out,
            "rtt min/avg/max = {}.{:03}/{}.{:03}/{}.{:03} ms",
            min_us / 1000,
            min_us % 1000,
            avg / 1000,
            avg % 1000,
            max_us / 1000,
            max_us % 1000
        );
    }
    Ok(u8::from(received != count))
}
pub fn socket() -> KResult<Arc<dyn FileLike>> {
    let mut guard = NETWORK.lock();
    let net = guard.as_mut().ok_or(Errno::ENETUNREACH)?;
    if net.states.len() >= MAX_SOCKETS {
        return Err(Errno::EMFILE);
    }
    let mut socket = tcp::Socket::new(
        tcp::SocketBuffer::new(vec![0; CAPACITY]),
        tcp::SocketBuffer::new(vec![0; CAPACITY]),
    );
    socket.set_timeout(Some(Duration::from_secs(20)));
    let handle = net.sockets.add(socket);
    let id = net.next_id;
    net.next_id += 1;
    net.states.insert(
        id,
        State {
            handle,
            connecting: false,
            connected: false,
            error: None,
            flags: 0,
            epochs: [1, 1],
            started_ns: 0,
        },
    );
    Ok(Arc::new(SocketFile(id)))
}
pub fn connect(id: usize, endpoint: Endpoint, nonblock: bool) -> KResult<()> {
    if nonblock {
        if !NETWORK
            .lock()
            .as_ref()
            .ok_or(Errno::ENETUNREACH)?
            .configured
        {
            return Err(Errno::ENETUNREACH);
        }
    } else {
        wait_ready()?;
    }
    {
        let mut guard = NETWORK.lock();
        let net = guard.as_mut().ok_or(Errno::ENETUNREACH)?;
        let state = net.states.get_mut(&id).ok_or(Errno::ENOTSOCK)?;
        if state.connecting {
            return Err(Errno::EALREADY);
        }
        if state.connected {
            return Err(Errno::EISCONN);
        }
        // Sequential ephemeral ports; skip all ports already held by a socket.
        let mut local = net.port;
        for _ in 0..MAX_SOCKETS + 1 {
            if !net.sockets.iter().any(|(_, s)| matches!(s, smoltcp::socket::Socket::Tcp(s) if s.local_endpoint().is_some_and(|e| e.port == local))) { break; }
            local = if local == 65535 { 49152 } else { local + 1 };
        }
        net.port = if local == 65535 { 49152 } else { local + 1 };
        net.sockets
            .get_mut::<tcp::Socket>(state.handle)
            .connect(
                net.interface.context(),
                IpEndpoint::new(
                    IpAddress::Ipv4(Ipv4Address::from(endpoint.ip)),
                    endpoint.port,
                ),
                local,
            )
            .map_err(|_| Errno::EINVAL)?;
        state.connecting = true;
        state.started_ns = time::monotonic_ns();
        state.error = None;
    }
    if nonblock {
        return Err(Errno::EINPROGRESS);
    }
    loop {
        {
            let guard = NETWORK.lock();
            let state = guard
                .as_ref()
                .unwrap()
                .states
                .get(&id)
                .ok_or(Errno::ENOTSOCK)?;
            if let Some(e) = state.error {
                return Err(e);
            }
            if state.connected {
                return Ok(());
            }
        }
        time::sleep_until(time::monotonic_ns() + 10_000_000)?;
    }
}
pub fn endpoint(id: usize, peer: bool) -> KResult<Endpoint> {
    let guard = NETWORK.lock();
    let net = guard.as_ref().ok_or(Errno::ENETUNREACH)?;
    let state = net.states.get(&id).ok_or(Errno::ENOTSOCK)?;
    let socket = net.sockets.get::<tcp::Socket>(state.handle);
    let e = if peer {
        socket.remote_endpoint()
    } else {
        socket.local_endpoint()
    };
    let e = e.ok_or(Errno::ENOTCONN)?;
    let IpAddress::Ipv4(ip) = e.addr;
    Ok(Endpoint {
        ip: ip.octets(),
        port: e.port,
    })
}
pub fn set_option(id: usize, level: u32, option: u32, value: i32) -> KResult<()> {
    let mut guard = NETWORK.lock();
    let net = guard.as_mut().ok_or(Errno::ENETUNREACH)?;
    let state = net.states.get(&id).ok_or(Errno::ENOTSOCK)?;
    let socket = net.sockets.get_mut::<tcp::Socket>(state.handle);
    match (level, option) {
        (6, 1) => socket.set_nagle_enabled(value == 0), // TCP_NODELAY
        (1, 9) => socket.set_keep_alive((value != 0).then_some(Duration::from_secs(15))),
        (6, 4) if value > 0 => socket.set_keep_alive(Some(Duration::from_secs(value as u64))),
        // smoltcp has one keepalive period, not separate idle/interval/probe counts.
        (6, 5 | 6) => return Err(Errno::ENOPROTOOPT),
        _ => return Err(Errno::ENOPROTOOPT),
    }
    Ok(())
}
pub fn socket_error(id: usize) -> KResult<u32> {
    let mut guard = NETWORK.lock();
    let net = guard.as_mut().ok_or(Errno::ENETUNREACH)?;
    let state = net.states.get_mut(&id).ok_or(Errno::ENOTSOCK)?;
    Ok(state.error.take().map_or(0, |e| e.code() as u32))
}
pub fn shutdown(id: usize, how: u32) -> KResult<()> {
    if how > 2 {
        return Err(Errno::EINVAL);
    }
    if how != 1 {
        return Err(Errno::EOPNOTSUPP);
    }
    let mut guard = NETWORK.lock();
    let net = guard.as_mut().ok_or(Errno::ENETUNREACH)?;
    let state = net.states.get_mut(&id).ok_or(Errno::ENOTSOCK)?;
    if !state.connected {
        return Err(Errno::ENOTCONN);
    }
    // smoltcp close sends FIN while continuing to receive the peer's data.
    if how != 0 {
        net.sockets.get_mut::<tcp::Socket>(state.handle).close();
    }
    Ok(())
}
struct SocketFile(usize);
impl Drop for SocketFile {
    fn drop(&mut self) {
        let mut guard = NETWORK.lock();
        if let Some(net) = guard.as_mut() {
            if let Some(state) = net.states.remove(&self.0) {
                net.sockets.get_mut::<tcp::Socket>(state.handle).abort();
                net.interface
                    .poll_egress(now(), &mut net.device, &mut net.sockets);
                net.sockets.remove(state.handle);
                if net.states.is_empty() {
                    net.states = BTreeMap::new();
                }
            }
        }
    }
}
impl SocketFile {
    fn transfer(
        &self,
        mut operation: impl FnMut(&mut tcp::Socket) -> KResult<usize>,
        nonblock: bool,
    ) -> KResult<usize> {
        loop {
            let result = {
                let mut guard = NETWORK.lock();
                let net = guard.as_mut().ok_or(Errno::ENETUNREACH)?;
                let state = net.states.get_mut(&self.0).ok_or(Errno::ENOTSOCK)?;
                if let Some(e) = state.error {
                    return Err(e);
                }
                if !state.connected {
                    return Err(Errno::ENOTCONN);
                }
                let socket = net.sockets.get_mut::<tcp::Socket>(state.handle);
                let result = operation(socket);
                update(state, socket);
                result
            };
            if result != Err(Errno::EAGAIN) || nonblock {
                return result;
            }
            time::sleep_until(time::monotonic_ns() + 10_000_000)?;
        }
    }
}
impl FileLike for SocketFile {
    fn socket_id(&self) -> Option<usize> {
        Some(self.0)
    }
    fn read(&self, _: u64, buf: &mut [u8], nonblock: bool) -> KResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.transfer(
            |s| {
                if s.can_recv() {
                    s.recv_slice(buf).map_err(|_| Errno::ECONNRESET)
                } else if !s.may_recv() {
                    Ok(0)
                } else {
                    Err(Errno::EAGAIN)
                }
            },
            nonblock,
        )
    }
    fn write_with_flags(&self, _: u64, buf: &[u8], nonblock: bool) -> KResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.transfer(
            |s| {
                if !s.may_send() {
                    Err(Errno::EPIPE)
                } else if s.can_send() {
                    s.send_slice(buf).map_err(|_| Errno::EPIPE)
                } else {
                    Err(Errno::EAGAIN)
                }
            },
            nonblock,
        )
    }
    fn poll(&self) -> Ready {
        let flags = self.poll_flags();
        Ready {
            readable: flags & 1 != 0,
            writable: flags & 4 != 0,
        }
    }
    fn poll_flags(&self) -> u32 {
        NETWORK
            .lock()
            .as_ref()
            .and_then(|n| n.states.get(&self.0))
            .map_or(0x18, |s| s.flags)
    }
    fn readiness_epochs(&self) -> [u64; 2] {
        NETWORK
            .lock()
            .as_ref()
            .and_then(|n| n.states.get(&self.0))
            .map_or([0, 0], |s| s.epochs)
    }
    fn epollable(&self) -> bool {
        true
    }
    fn stat(&self) -> Stat {
        Stat {
            ino: self.0 as u64,
            file_type: FileType::Socket,
            permissions: 0o600,
            size: 0,
            nlink: 1,
            rdev: 0,
        }
    }
}
