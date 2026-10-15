//! Dispatch Ethernet devices without tying the protocol stack to one NIC.
use super::{e1000e, pci, qca6174, rtl8168};
use smoltcp::{
    phy::{Device, DeviceCapabilities, RxToken, TxToken},
    time::Instant,
};

pub enum Ethernet {
    Intel(e1000e::E1000e),
    Realtek(rtl8168::Rtl8168),
    Wireless(qca6174::Wireless),
}
impl Ethernet {
    pub fn new(device: pci::Device) -> Option<Self> {
        if rtl8168::Rtl8168::matches(&device) {
            match rtl8168::Rtl8168::new(device) {
                Ok(nic) => Some(Self::Realtek(nic)),
                Err(reason) => {
                    crate::serial_println!(
                        "NET: Realtek {:04x}:{:04x} rev {:02x}: {}",
                        device.vendor,
                        device.device,
                        device.revision,
                        reason
                    );
                    crate::println!(
                        "NET: Realtek {:04x}:{:04x} rev {:02x}: {}",
                        device.vendor,
                        device.device,
                        device.revision,
                        reason
                    );
                    None
                }
            }
        } else {
            e1000e::E1000e::new(device).map(Self::Intel)
        }
    }
    pub fn name(&self) -> &'static str {
        match self {
            Self::Intel(_) => "Intel 82574L",
            Self::Realtek(n) => n.name(),
            Self::Wireless(_) => "WiFi QCA6174",
        }
    }
    pub fn mac(&self) -> [u8; 6] {
        match self {
            Self::Intel(n) => n.mac,
            Self::Realtek(n) => n.mac,
            Self::Wireless(n) => n.mac(),
        }
    }
    pub fn link(&self) -> bool {
        match self {
            Self::Intel(n) => n.link(),
            Self::Realtek(n) => n.link(),
            Self::Wireless(n) => n.link(),
        }
    }
    pub fn counters(&self) -> (u64, u64, u64) {
        match self {
            Self::Intel(n) => (n.received, n.sent, 0),
            Self::Realtek(n) => (n.received, n.sent, n.dropped),
            Self::Wireless(n) => n.counters(),
        }
    }
}
pub enum Receive {
    Intel(e1000e::Receive),
    Realtek(rtl8168::Receive),
    Wireless(qca6174::WifiReceive),
}
impl RxToken for Receive {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        match self {
            Self::Intel(token) => token.consume(f),
            Self::Realtek(token) => token.consume(f),
            Self::Wireless(token) => token.consume(f),
        }
    }
}
pub enum Transmit<'a> {
    Intel(e1000e::Transmit<'a>),
    Realtek(rtl8168::Transmit<'a>),
    Wireless(qca6174::WifiTransmit),
}
impl TxToken for Transmit<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        match self {
            Self::Intel(token) => token.consume(len, f),
            Self::Realtek(token) => token.consume(len, f),
            Self::Wireless(token) => token.consume(len, f),
        }
    }
}
impl Device for Ethernet {
    type RxToken<'a> = Receive;
    type TxToken<'a> = Transmit<'a>;
    fn receive(&mut self, time: Instant) -> Option<(Receive, Transmit<'_>)> {
        match self {
            Self::Intel(n) => n
                .receive(time)
                .map(|(rx, tx)| (Receive::Intel(rx), Transmit::Intel(tx))),
            Self::Realtek(n) => n
                .receive(time)
                .map(|(rx, tx)| (Receive::Realtek(rx), Transmit::Realtek(tx))),
            Self::Wireless(n) => n
                .receive(time)
                .map(|(rx, tx)| (Receive::Wireless(rx), Transmit::Wireless(tx))),
        }
    }
    fn transmit(&mut self, time: Instant) -> Option<Transmit<'_>> {
        match self {
            Self::Intel(n) => n.transmit(time).map(Transmit::Intel),
            Self::Realtek(n) => n.transmit(time).map(Transmit::Realtek),
            Self::Wireless(n) => n.transmit(time).map(Transmit::Wireless),
        }
    }
    fn capabilities(&self) -> DeviceCapabilities {
        match self {
            Self::Intel(n) => n.capabilities(),
            Self::Realtek(n) => n.capabilities(),
            Self::Wireless(n) => n.capabilities(),
        }
    }
}
