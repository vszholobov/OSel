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
    socket::{dhcpv4, tcp},
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
        crate::serial_println!("NET: no supported Ethernet card (Intel 82574L or Realtek RTL8168 G/GU/H/M)");
        return;
    };
    let mac = device.mac();
    let mut config = Config::new(EthernetAddress(mac).into());
    config.random_seed = time::realtime_ns()
        ^ u64::from_le_bytes([
            mac[0],
            mac[1],
            mac[2],
            mac[3],
            mac[4],
            mac[5],
            0,
            0,
        ]);
    let interface = Interface::new(config, &mut device, now());
    let storage = (0..MAX_SOCKETS + 1)
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
        let _ = writeln!(out, "No initialized Ethernet card: Intel 8086:10d3 or Realtek 10ec:8168 (G/GU/H/M); see NET boot diagnostics");
    }
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
