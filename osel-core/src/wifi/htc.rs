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
}
