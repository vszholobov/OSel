//! Realtek descriptor format and MAC identification; no hardware access.
pub const OWN: u32 = 1 << 31;
pub const EOR: u32 = 1 << 30;
pub const FIRST: u32 = 1 << 29;
pub const LAST: u32 = 1 << 28;
pub const BUFFER_SIZE: usize = 2048;
pub const RING_SIZE: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chip {
    Cplus,
    G,
    Gu,
    H,
    M,
}
impl Chip {
    /// PCI revision is not a MAC version. Identify the read-only TxConfig XID.
    pub fn detect(vendor: u16, device: u16, tx_config: u32) -> Option<Self> {
        if vendor != 0x10ec {
            return None;
        }
        let xid = (tx_config >> 20) & 0x7cf;
        match (device, xid) {
            (0x8139, 0x748) => Some(Self::Cplus),
            (0x8168, 0x4c0) => Some(Self::G),
            (0x8168, 0x509) => Some(Self::Gu),
            (0x8168, 0x541) => Some(Self::H),
            (0x8168, 0x6c0) => Some(Self::M),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Cplus => "Realtek RTL8139 C+",
            Self::G => "Realtek RTL8168G/8111G",
            Self::Gu => "Realtek RTL8168GU/8111GU",
            Self::H => "Realtek RTL8168H/8111H",
            Self::M => "Realtek RTL8168M",
        }
    }
    pub fn gigabit(self) -> bool {
        self != Self::Cplus
    }
    /// Length includes the Ethernet FCS. GBE error flags sit one bit above C+.
    pub fn received_len(self, status: u32) -> Option<usize> {
        if status & (OWN | FIRST | LAST) != FIRST | LAST {
            return None;
        }
        let error_mask = if self.gigabit() {
            0x07f8_0000
        } else {
            0x08fc_0000
        };
        // Multicast/unicast/broadcast marks are not errors.
        let error_mask = error_mask & !0x0700_0000;
        if status & error_mask != 0 {
            return None;
        }
        let wire_len = (status & if self.gigabit() { 0x3fff } else { 0x1fff }) as usize;
        if !(18..=1518).contains(&wire_len) {
            return None;
        }
        Some(wire_len - 4)
    }
}
pub fn ring_end(index: usize) -> u32 {
    if index + 1 == RING_SIZE { EOR } else { 0 }
}
pub fn receive_command(index: usize) -> u32 {
    OWN | ring_end(index) | BUFFER_SIZE as u32
}
pub fn transmit_command(index: usize, len: usize) -> Option<u32> {
    (14..=1514)
        .contains(&len)
        .then_some(OWN | FIRST | LAST | ring_end(index) | len as u32)
}

/// Small hardware boundary lets host tests exercise the actual G/H/M PHY path.
pub trait PhyIo {
    fn read8(&self, offset: usize) -> u8;
    fn write8(&self, offset: usize, value: u8);
    fn read32(&self, offset: usize) -> u32;
    fn write32(&self, offset: usize, value: u32);
    fn delay_us(&self, us: u16) -> Result<(), &'static str>;
}
fn phy(io: &impl PhyIo, reg: u16, write: Option<u16>) -> Result<u16, &'static str> {
    let command = ((0xa400 + reg as u32 * 2) << 15) | write.map_or(0, |v| OWN | v as u32);
    io.write32(0xb8, command);
    for _ in 0..100 {
        io.delay_us(25)?;
        let value = io.read32(0xb8);
        if (value & OWN != 0) == write.is_none() {
            io.delay_us(20)?;
            return Ok(value as u16);
        }
    }
    Err("PHY OCP access timed out")
}
pub fn configure_phy(io: &impl PhyIo) -> Result<(), &'static str> {
    io.write8(0x6f, io.read8(0x6f) | 0xc0); // wake PHY/PLL
    io.delay_us(1000)?;
    phy(io, 0, Some(0x8000))?;
    let mut reset = false;
    for _ in 0..100 {
        if phy(io, 0, None)? & 0x8000 == 0 {
            reset = true;
            break;
        }
        io.delay_us(1000)?;
    }
    if !reset {
        return Err("PHY reset timed out");
    }
    // IEEE 802.3 auto negotiation: 10/100 and 1000BASE-T full duplex.
    phy(io, 4, Some(0x01e1))?;
    phy(io, 9, Some(0x0200))?;
    phy(io, 0, Some(0x1200))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    struct PhyModel {
        result: Cell<u32>,
        registers: RefCell<[u16; 32]>,
        writes: RefCell<Vec<u32>>,
        stuck_access: bool,
        stuck_reset: bool,
        delay_failure: bool,
    }
    impl PhyModel {
        fn new() -> Self {
            Self {
                result: Cell::new(0),
                registers: RefCell::new([0; 32]),
                writes: RefCell::new(Vec::new()),
                stuck_access: false,
                stuck_reset: false,
                delay_failure: false,
            }
        }
    }
    impl PhyIo for PhyModel {
        fn read8(&self, _: usize) -> u8 {
            0
        }
        fn write8(&self, _: usize, _: u8) {}
        fn read32(&self, _: usize) -> u32 {
            self.result.get()
        }
        fn write32(&self, offset: usize, command: u32) {
            assert_eq!(offset, 0xb8);
            self.writes.borrow_mut().push(command);
            let address = ((command & !OWN) >> 15) as u16 & !1;
            assert!((0xa400..=0xa43e).contains(&address));
            let index = ((address - 0xa400) / 2) as usize;
            if self.stuck_access {
                self.result.set(command);
                return;
            }
            if command & OWN != 0 {
                let mut value = command as u16;
                if index == 0 && !self.stuck_reset {
                    value &= !0x8000;
                }
                self.registers.borrow_mut()[index] = value;
                self.result.set(value as u32);
            } else {
                self.result.set(OWN | self.registers.borrow()[index] as u32);
            }
        }
        fn delay_us(&self, _: u16) -> Result<(), &'static str> {
            if self.delay_failure {
                Err("timer failed")
            } else {
                Ok(())
            }
        }
    }
    #[test]
    fn gigabit_phy_uses_ocp_and_restarts_negotiation() {
        let io = PhyModel::new();
        assert_eq!(configure_phy(&io), Ok(()));
        assert_eq!(io.registers.borrow()[0], 0x1200);
        assert_eq!(io.registers.borrow()[4], 0x01e1);
        assert_eq!(io.registers.borrow()[9], 0x0200);
        assert_eq!(io.writes.borrow().last(), Some(&0xd200_1200));
    }
    #[test]
    fn phy_access_reset_and_timer_failures_are_bounded() {
        let mut io = PhyModel::new();
        io.stuck_access = true;
        assert_eq!(configure_phy(&io), Err("PHY OCP access timed out"));
        assert_eq!(io.writes.borrow().len(), 1);
        let mut io = PhyModel::new();
        io.stuck_reset = true;
        assert_eq!(configure_phy(&io), Err("PHY reset timed out"));
        assert_eq!(io.writes.borrow().len(), 101);
        let mut io = PhyModel::new();
        io.delay_failure = true;
        assert_eq!(configure_phy(&io), Err("timer failed"));
        assert!(io.writes.borrow().is_empty());
    }
    #[test]
    fn refuses_unknown_mac_even_with_matching_pci_id() {
        assert_eq!(Chip::detect(0x10ec, 0x8168, 0x5410_0700), Some(Chip::H));
        assert_eq!(Chip::detect(0x10ec, 0x8168, 0x6c00_0700), Some(Chip::M));
        assert_eq!(Chip::detect(0x10ec, 0x8168, 0xffff_ffff), None);
        assert_eq!(Chip::detect(0x10ec, 0x8168, 0x2c00_0000), None);
        assert_eq!(Chip::detect(0x10ec, 0x8139, 0x7480_0000), Some(Chip::Cplus));
        assert_eq!(Chip::detect(0x10ec, 0x8139, 0x7400_0000), None);
        assert_eq!(Chip::detect(0x8086, 0x8168, 0x5410_0000), None);
    }
    #[test]
    fn only_the_last_descriptor_wraps_the_ring() {
        for i in 0..RING_SIZE {
            assert_eq!(receive_command(i) & EOR != 0, i == RING_SIZE - 1);
            assert_eq!(
                transmit_command(i, 1514).unwrap() & EOR != 0,
                i == RING_SIZE - 1
            );
        }
        assert!(transmit_command(0, 1515).is_none());
        assert!(transmit_command(0, 0).is_none());
    }
    #[test]
    fn validates_ownership_fragments_errors_and_crc_length() {
        for chip in [Chip::Cplus, Chip::G, Chip::Gu, Chip::H, Chip::M] {
            let packet = FIRST | LAST | EOR | 1518;
            assert_eq!(chip.received_len(packet), Some(1514));
            assert_eq!(chip.received_len(packet | OWN), None);
            assert_eq!(chip.received_len(packet & !FIRST), None);
            assert_eq!(chip.received_len(packet & !LAST), None);
            assert_eq!(chip.received_len(FIRST | LAST | 17), None);
            assert_eq!(chip.received_len(FIRST | LAST | 1519), None);
            assert_eq!(chip.received_len(packet | (1 << 24)), Some(1514));
            for bit in if chip.gigabit() {
                [19, 20, 21]
            } else {
                [18, 19, 20]
            } {
                assert_eq!(chip.received_len(packet | (1 << bit)), None);
            }
        }
    }
}
