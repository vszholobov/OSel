//! Linux IPv4 socket addresses and PCI configuration decoding, independent of hardware.
use crate::errno::{Errno, KResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpoint {
    pub ip: [u8; 4],
    pub port: u16,
}
impl Endpoint {
    pub fn decode(bytes: &[u8]) -> KResult<Self> {
        if bytes.len() < 16 {
            return Err(Errno::EINVAL);
        }
        if u16::from_le_bytes([bytes[0], bytes[1]]) != 2 {
            return Err(Errno::EAFNOSUPPORT);
        }
        Ok(Self {
            ip: bytes[4..8].try_into().unwrap(),
            port: u16::from_be_bytes([bytes[2], bytes[3]]),
        })
    }
    pub fn encode(self) -> [u8; 16] {
        let mut bytes = [0; 16];
        bytes[..2].copy_from_slice(&2u16.to_le_bytes());
        bytes[2..4].copy_from_slice(&self.port.to_be_bytes());
        bytes[4..8].copy_from_slice(&self.ip);
        bytes
    }
}

pub const fn pci_address(bus: u8, slot: u8, function: u8, offset: u8) -> u32 {
    0x80000000
        | ((bus as u32) << 16)
        | ((slot as u32 & 31) << 11)
        | ((function as u32 & 7) << 8)
        | (offset as u32 & 0xfc)
}

/// Memory BAR, including the upper dword of a 64-bit BAR. I/O and reserved BARs fail.
pub fn memory_bar(low: u32, high: u32) -> Option<u64> {
    if low & 1 != 0 {
        return None;
    }
    let address = match low & 6 {
        0 => (low & !15) as u64,
        4 => ((high as u64) << 32) | (low & !15) as u64,
        _ => return None,
    };
    (address != 0).then_some(address)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn socket_addresses_use_mixed_endianness() {
        let a = Endpoint {
            ip: [10, 0, 2, 2],
            port: 8080,
        };
        assert_eq!(&a.encode()[..8], &[2, 0, 0x1f, 0x90, 10, 0, 2, 2]);
        assert_eq!(Endpoint::decode(&a.encode()), Ok(a));
        assert_eq!(Endpoint::decode(&[0; 15]), Err(Errno::EINVAL));
        assert_eq!(Endpoint::decode(&[0; 16]), Err(Errno::EAFNOSUPPORT));
    }
    #[test]
    fn pci_encoding_and_bars() {
        assert_eq!(pci_address(3, 4, 2, 0x13), 0x80032210);
        assert_eq!(memory_bar(0xfea00000, 0), Some(0xfea00000));
        assert_eq!(memory_bar(0xfea00004, 1), Some(0x1fea00000));
        assert_eq!(memory_bar(0x1001, 0), None);
        assert_eq!(memory_bar(6, 0), None);
    }
}
