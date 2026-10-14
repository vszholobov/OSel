//! ICMP echo packets, independent of the device and network stack.
pub const PACKET_LEN: usize = 40;

fn checksum(bytes: &[u8]) -> u16 {
    let mut sum = 0u32;
    for word in bytes.chunks(2) {
        sum += u32::from(u16::from_be_bytes([word[0], *word.get(1).unwrap_or(&0)]));
    }
    while sum > 0xffff {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

pub fn request(ident: u16, seq: u16, nonce: u64) -> [u8; PACKET_LEN] {
    let mut bytes = [0u8; PACKET_LEN];
    bytes[0] = 8;
    bytes[4..6].copy_from_slice(&ident.to_be_bytes());
    bytes[6..8].copy_from_slice(&seq.to_be_bytes());
    bytes[8..16].copy_from_slice(&nonce.to_be_bytes());
    for (i, byte) in bytes[16..].iter_mut().enumerate() {
        *byte = i as u8;
    }
    let sum = checksum(&bytes);
    bytes[2..4].copy_from_slice(&sum.to_be_bytes());
    bytes
}

/// A stale, corrupt, truncated or unrelated packet cannot count as a reply.
pub fn reply_matches(bytes: &[u8], ident: u16, seq: u16, nonce: u64) -> bool {
    if bytes.len() != PACKET_LEN || bytes[0] != 0 || bytes[1] != 0 || checksum(bytes) != 0 {
        return false;
    }
    let expected = request(ident, seq, nonce);
    bytes[4..] == expected[4..]
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reply(id: u16, seq: u16, nonce: u64) -> [u8; PACKET_LEN] {
        let mut bytes = request(id, seq, nonce);
        bytes[0] = 0;
        bytes[2..4].fill(0);
        let sum = checksum(&bytes);
        bytes[2..4].copy_from_slice(&sum.to_be_bytes());
        bytes
    }
    #[test]
    fn validates_complete_echo_identity_and_payload() {
        let good = reply(0x1234, 3, 99);
        assert!(reply_matches(&good, 0x1234, 3, 99));
        assert!(!reply_matches(&good, 0x1234, 4, 99));
        assert!(!reply_matches(&good, 0x1234, 3, 100));
        assert!(!reply_matches(&good[..16], 0x1234, 3, 99));
        assert!(!reply_matches(&request(0x1234, 3, 99), 0x1234, 3, 99));
        for i in 0..PACKET_LEN {
            let mut bad = good;
            bad[i] ^= 1;
            assert!(!reply_matches(&bad, 0x1234, 3, 99));
        }
    }
    #[test]
    fn rfc_checksum_includes_odd_final_byte() {
        assert_eq!(checksum(&[0, 1, 0xf2, 3, 0xf4, 0xf5, 0xf6, 0xf7]), 0x220d);
        assert_eq!(checksum(&[1]), 0xfeff);
        assert_eq!(checksum(&request(1, 2, 3)), 0);
    }
}
