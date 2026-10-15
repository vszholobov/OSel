// SPDX-License-Identifier: ISC
// HTC ABI from Linux ath10k v6.12, see firmware/ath10k-driver-ISC.txt.
//! Checked first firmware response; it does not imply WMI or a Wi-Fi connection.
use crate::errno::{Errno, KResult};
#[derive(Debug, PartialEq, Eq)]
pub struct Ready {
    pub credits: u16,
    pub credit_size: u16,
    pub endpoints: u8,
}
pub fn payload(packet: &[u8]) -> KResult<&[u8]> {
    if packet.len() < 8 || packet[0] >= 9 {
        return Err(Errno::EIO);
    }
    let length = u16::from_le_bytes([packet[2], packet[3]]) as usize;
    if length + 8 > packet.len() {
        return Err(Errno::EIO);
    }
    let trailer = if packet[1] & 2 != 0 {
        packet[4] as usize
    } else {
        0
    };
    if trailer > length || (packet[1] & 2 != 0 && trailer < 2) {
        return Err(Errno::EIO);
    }
    Ok(&packet[8..8 + length - trailer])
}
#[derive(Debug, PartialEq, Eq)]
pub struct Service {
    pub endpoint: u8,
    pub max_message: u16,
}
pub fn connect(service: u16, credits: u16, sequence: u8) -> KResult<[u8; 16]> {
    let flags = match service {
        0x100 if credits > 0 && credits <= 255 => credits << 8,
        0x300 => 8, // HTT has no HTC credit flow control
        _ => return Err(Errno::EOPNOTSUPP),
    };
    let mut packet = [0u8; 16];
    packet[2] = 8;
    packet[5] = sequence;
    packet[8..10].copy_from_slice(&2u16.to_le_bytes());
    packet[10..12].copy_from_slice(&service.to_le_bytes());
    packet[12..14].copy_from_slice(&flags.to_le_bytes());
    Ok(packet)
}
pub fn connected(packet: &[u8], service: u16) -> KResult<Service> {
    let data = payload(packet)?;
    if packet[0] != 0
        || data.len() < 8
        || data[..2] != [3, 0]
        || u16::from_le_bytes([data[2], data[3]]) != service
        || data[4] != 0
        || data[5] == 0
        || data[5] >= 9
    {
        return Err(Errno::EIO);
    }
    let max_message = u16::from_le_bytes([data[6], data[7]]);
    if max_message == 0 || max_message > 4096 {
        return Err(Errno::EIO);
    }
    Ok(Service {
        endpoint: data[5],
        max_message,
    })
}
pub fn setup(sequence: u8) -> [u8; 20] {
    let mut packet = [0; 20];
    packet[2] = 12;
    packet[5] = sequence;
    packet[8] = 5; // SETUP_COMPLETE_EX, no RX bundling on PCI
    packet
}
pub fn ready(packet: &[u8]) -> KResult<Ready> {
    if packet.len() < 16 || packet[0] != 0 {
        return Err(Errno::EIO);
    }
    let length = u16::from_le_bytes([packet[2], packet[3]]) as usize;
    if length < 8 || length + 8 > packet.len() {
        return Err(Errno::EIO);
    }
    let trailer = if packet[1] & 2 != 0 {
        packet[4] as usize
    } else {
        0
    };
    if trailer > length || (packet[1] & 2 != 0 && trailer < 2) || length - trailer < 8 {
        return Err(Errno::EIO);
    }
    let data = &packet[8..8 + length - trailer];
    if u16::from_le_bytes([data[0], data[1]]) != 1 {
        return Err(Errno::EIO);
    }
    let value = Ready {
        credits: u16::from_le_bytes([data[2], data[3]]),
        credit_size: u16::from_le_bytes([data[4], data[5]]),
        endpoints: data[6],
    };
    if value.credits == 0 || value.credit_size == 0 || value.endpoints == 0 || value.endpoints > 9 {
        return Err(Errno::EIO);
    }
    Ok(value)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn htc_ready_requires_a_complete_valid_endpoint_zero_message() {
        let packet = [0, 0, 8, 0, 0, 0, 0, 0, 1, 0, 32, 0, 0, 1, 9, 0];
        assert_eq!(
            ready(&packet),
            Ok(Ready {
                credits: 32,
                credit_size: 256,
                endpoints: 9
            })
        );
        for end in 0..packet.len() {
            assert!(ready(&packet[..end]).is_err());
        }
        for (index, value) in [
            (0, 1),
            (2, 7),
            (2, 40),
            (8, 2),
            (10, 0),
            (13, 0),
            (14, 0),
            (14, 10),
        ] {
            let mut bad = packet;
            bad[index] = value;
            assert!(ready(&bad).is_err(), "index {index}, value {value}");
        }
    }
    #[test]
    fn connect_messages_validate_service_status_endpoint_and_trailers() {
        let wmi = connect(0x100, 8, 2).unwrap();
        assert_eq!(&wmi[8..14], &[2, 0, 0, 1, 0, 8]);
        assert_eq!(wmi[5], 2);
        assert!(connect(0x100, 256, 0).is_err());
        let mut reply = [0, 0, 8, 0, 0, 0, 0, 0, 3, 0, 0, 1, 0, 2, 0, 8];
        assert_eq!(
            connected(&reply, 0x100),
            Ok(Service {
                endpoint: 2,
                max_message: 2048
            })
        );
        assert!(connected(&reply, 0x300).is_err());
        reply[12] = 1;
        assert!(connected(&reply, 0x100).is_err());
        reply[12] = 0;
        reply[13] = 0;
        assert!(connected(&reply, 0x100).is_err());
        reply[13] = 2;
        reply[1] = 2;
        reply[4] = 20;
        assert!(connected(&reply, 0x100).is_err());
        assert_eq!(payload(&setup(1)).unwrap().len(), 12);
    }
}
