//! WPA2-Personal/CCMP EAPOL state machine. No I/O and no non-cryptographic RNG fallback.
//! Primitives are RustCrypto; wire inputs must come from the associated BSSID.
use super::ieee80211::{self, RSN_IE};
use crate::errno::{Errno, KResult};
use alloc::{vec, vec::Vec};
use hmac::{Hmac, Mac};
use sha1::Sha1;
use zeroize::Zeroizing;

type HmacSha1 = Hmac<Sha1>;
pub type Pmk = Zeroizing<[u8; 32]>;
pub type Password = Zeroizing<Vec<u8>>;
/// Hidden input consumes the entire invalid line before returning an error, so
/// an overlong password suffix cannot become visible shell input.
pub struct PasswordReader {
    bytes: Password,
    invalid: bool,
    finished: bool,
}
impl PasswordReader {
    pub fn new() -> Self {
        Self {
            bytes: Password::new(Vec::with_capacity(64)),
            invalid: false,
            finished: false,
        }
    }
    pub fn receive(&mut self, byte: u8) -> Option<KResult<Password>> {
        if self.finished {
            return None;
        }
        match byte {
            3 => {
                self.finished = true;
                return Some(Err(Errno::EINTR));
            }
            b'\r' | b'\n' => {
                self.finished = true;
                return Some(if self.invalid {
                    Err(Errno::EINVAL)
                } else {
                    Ok(core::mem::replace(
                        &mut self.bytes,
                        Password::new(Vec::new()),
                    ))
                });
            }
            8 | 127 => {
                self.bytes.pop();
            }
            0x20..=0xff if self.bytes.len() < 64 => self.bytes.push(byte),
            _ => self.invalid = true,
        }
        None
    }
}

pub fn derive(password: &[u8], ssid: &[u8]) -> KResult<Pmk> {
    if ssid.is_empty() || ssid.len() > 32 {
        return Err(Errno::EINVAL);
    }
    let mut key = Zeroizing::new([0; 32]);
    if password.len() == 64 {
        fn digit(byte: u8) -> KResult<u8> {
            match byte {
                b'0'..=b'9' => Ok(byte - b'0'),
                b'a'..=b'f' => Ok(byte - b'a' + 10),
                b'A'..=b'F' => Ok(byte - b'A' + 10),
                _ => Err(Errno::EINVAL),
            }
        }
        for (i, pair) in password.chunks_exact(2).enumerate() {
            key[i] = digit(pair[0])? * 16 + digit(pair[1])?;
        }
    } else {
        if !(8..=63).contains(&password.len())
            || password.iter().any(|&byte| byte < 0x20 || byte == 0x7f)
        {
            return Err(Errno::EINVAL);
        }
        pbkdf2::pbkdf2_hmac::<Sha1>(password, ssid, 4096, &mut *key);
    }
    Ok(key)
}
fn ptk(
    pmk: &Pmk,
    station: [u8; 6],
    ap: [u8; 6],
    snonce: &[u8; 32],
    anonce: &[u8; 32],
) -> Zeroizing<[u8; 48]> {
    let (mac1, mac2) = if station < ap {
        (station, ap)
    } else {
        (ap, station)
    };
    let (nonce1, nonce2) = if snonce < anonce {
        (snonce, anonce)
    } else {
        (anonce, snonce)
    };
    let mut result = Zeroizing::new([0; 48]);
    for counter in 0..3 {
        let mut mac = HmacSha1::new_from_slice(&**pmk).unwrap();
        mac.update(b"Pairwise key expansion\0");
        mac.update(&mac1);
        mac.update(&mac2);
        mac.update(nonce1);
        mac.update(nonce2);
        mac.update(&[counter as u8]);
        let hash = mac.finalize().into_bytes();
        let start = counter * 20;
        let length = (48 - start).min(20);
        result[start..start + length].copy_from_slice(&hash[..length]);
    }
    result
}
struct KeyFrame<'a> {
    raw: &'a [u8],
    info: u16,
    replay: u64,
    nonce: [u8; 32],
    data: &'a [u8],
    rsc: [u8; 8],
}
impl<'a> KeyFrame<'a> {
    fn parse(packet: &'a [u8]) -> KResult<Self> {
        if packet.len() < 99 || !(1..=3).contains(&packet[0]) || packet[1] != 3 || packet[4] != 2 {
            return Err(Errno::EIO);
        }
        let length = u16::from_be_bytes([packet[2], packet[3]]) as usize + 4;
        let raw = packet
            .get(..length)
            .filter(|raw| raw.len() >= 99 && raw.len() <= 2048)
            .ok_or(Errno::EIO)?;
        let info = u16::from_be_bytes([raw[5], raw[6]]);
        if info & 7 != 2 || info & 0xec30 != 0 {
            return Err(Errno::EOPNOTSUPP);
        }
        let key_length = u16::from_be_bytes([raw[7], raw[8]]);
        if (info & 8 != 0 && key_length != 16)
            || (info & 8 == 0 && key_length != 0 && key_length != 16)
        {
            return Err(Errno::EOPNOTSUPP);
        }
        let data_len = u16::from_be_bytes([raw[97], raw[98]]) as usize;
        if 99 + data_len != raw.len() {
            return Err(Errno::EIO);
        }
        Ok(Self {
            raw,
            info,
            replay: u64::from_be_bytes(raw[9..17].try_into().unwrap()),
            nonce: raw[17..49].try_into().unwrap(),
            data: &raw[99..],
            rsc: raw[65..73].try_into().unwrap(),
        })
    }
    fn verify(&self, kck: &[u8]) -> KResult<()> {
        let mut mac = HmacSha1::new_from_slice(kck).unwrap();
        mac.update(&self.raw[..81]);
        mac.update(&[0; 16]);
        mac.update(&self.raw[97..]);
        mac.verify_truncated_left(&self.raw[81..97])
            .map_err(|_| Errno::EACCES)
    }
}
fn response(
    version: u8,
    info: u16,
    replay: u64,
    nonce: &[u8; 32],
    data: &[u8],
    kck: &[u8],
) -> Vec<u8> {
    let mut output = vec![0; 99 + data.len()];
    output[0] = version;
    output[1] = 3;
    output[2..4].copy_from_slice(&((95 + data.len()) as u16).to_be_bytes());
    output[4] = 2;
    output[5..7].copy_from_slice(&info.to_be_bytes());
    // RSN responses have key_length zero. M4/group replies also have a zero nonce.
    output[9..17].copy_from_slice(&replay.to_be_bytes());
    output[17..49].copy_from_slice(nonce);
    output[97..99].copy_from_slice(&(data.len() as u16).to_be_bytes());
    output[99..].copy_from_slice(data);
    let mut mac = HmacSha1::new_from_slice(kck).unwrap();
    mac.update(&output);
    output[81..97].copy_from_slice(&mac.finalize().into_bytes()[..16]);
    output
}
pub struct GroupKey {
    pub index: u8,
    pub rsc: [u8; 8],
    pub key: Zeroizing<[u8; 16]>,
}
pub struct Keys {
    pub pairwise: Option<Zeroizing<[u8; 16]>>,
    pub group: Option<GroupKey>,
}
pub enum Action {
    Reply(Vec<u8>),
    Install { keys: Keys, reply: Vec<u8> },
}
fn key_data(frame: &KeyFrame<'_>, kek: &[u8]) -> KResult<Zeroizing<Vec<u8>>> {
    if frame.info & 0x1000 == 0 {
        return Ok(Zeroizing::new(frame.data.to_vec()));
    }
    unwrap_key_data(kek.try_into().map_err(|_| Errno::EIO)?, frame.data)
}
pub fn unwrap_key_data(kek: &[u8; 16], data: &[u8]) -> KResult<Zeroizing<Vec<u8>>> {
    if data.len() < 24 || data.len() > 2048 || data.len() % 8 != 0 {
        return Err(Errno::EIO);
    }
    let mut plain = Zeroizing::new(vec![0u8; data.len() - 8]);
    let wrapper = aes_kw::Kek::<aes::Aes128>::new(kek.into());
    wrapper
        .unwrap(data, &mut plain)
        .map_err(|_| Errno::EACCES)?;
    Ok(plain)
}
fn gtk(data: &[u8], rsc: [u8; 8], require_rsn: bool, encrypted: bool) -> KResult<Option<GroupKey>> {
    let mut input = data;
    let mut group = None;
    let mut rsn = false;
    while !input.is_empty() {
        if input[0] == 0xdd && (input.len() == 1 || input[1] == 0) {
            if input[1..].iter().any(|&byte| byte != 0) {
                return Err(Errno::EIO);
            }
            break;
        }
        if input.len() < 2 {
            return Err(Errno::EIO);
        }
        let value = input.get(2..2 + input[1] as usize).ok_or(Errno::EIO)?;
        if input[0] == 48 {
            if rsn || !ieee80211::rsn(value)?.supported() {
                return Err(Errno::EACCES);
            }
            rsn = true;
        }
        if input[0] == 0xdd && value.starts_with(&[0, 0x0f, 0xac, 1]) {
            if !encrypted || group.is_some() || value.len() != 22 {
                return Err(Errno::EACCES);
            }
            group = Some(GroupKey {
                index: value[4] & 3,
                rsc,
                key: Zeroizing::new(value[6..22].try_into().unwrap()),
            });
        }
        input = &input[2 + value.len()..];
    }
    if require_rsn && !rsn {
        return Err(Errno::EACCES);
    }
    Ok(group)
}
/// Hardware installation and M4 TX must succeed before the caller calls installed().
/// A retransmitted M3/group message returns Reply, never another key installation.
pub struct Handshake {
    pmk: Pmk,
    station: [u8; 6],
    ap: [u8; 6],
    snonce: Zeroizing<[u8; 32]>,
    anonce: [u8; 32],
    ptk: Option<Zeroizing<[u8; 48]>>,
    m1: Option<u64>,
    last_replay: Option<u64>,
    accepted: Option<(u64, [u8; 16], bool)>,
    pending: bool,
    authorized: bool,
    group: Option<(u8, Zeroizing<[u8; 16]>)>,
}
impl Handshake {
    pub fn new(pmk: Pmk, station: [u8; 6], ap: [u8; 6]) -> KResult<Self> {
        if station == [0; 6]
            || ap == [0; 6]
            || station[0] & 1 != 0
            || ap[0] & 1 != 0
            || station == ap
        {
            return Err(Errno::EINVAL);
        }
        Ok(Self {
            pmk,
            station,
            ap,
            snonce: Zeroizing::new([0; 32]),
            anonce: [0; 32],
            ptk: None,
            m1: None,
            last_replay: None,
            accepted: None,
            pending: false,
            authorized: false,
            group: None,
        })
    }
    pub fn authorized(&self) -> bool {
        self.authorized
    }
    pub fn installed(&mut self) -> KResult<()> {
        if !self.pending {
            return Err(Errno::EALREADY);
        }
        self.pending = false;
        self.authorized = true;
        Ok(())
    }
    pub fn receive(
        &mut self,
        packet: &[u8],
        mut random: impl FnMut(&mut [u8; 32]) -> KResult<()>,
    ) -> KResult<Action> {
        let frame = KeyFrame::parse(packet)?;
        if frame.info & 0x80 == 0 {
            return Err(Errno::EACCES);
        }
        let pairwise = frame.info & 8 != 0;
        if pairwise && frame.info & 0x100 == 0 {
            if frame.info & (0x40 | 0x200 | 0x1000) != 0 || frame.nonce == [0; 32] {
                return Err(Errno::EACCES);
            }
            let duplicate = self.m1 == Some(frame.replay)
                && self.anonce == frame.nonce
                && self.accepted.is_none();
            if !duplicate {
                if self.last_replay.is_some_and(|old| frame.replay <= old) || self.pending {
                    return Err(Errno::EACCES);
                }
                let mut nonce = Zeroizing::new([0; 32]);
                random(&mut nonce)?;
                if *nonce == [0; 32] || *nonce == *self.snonce {
                    return Err(Errno::EIO);
                }
                let material = ptk(&self.pmk, self.station, self.ap, &nonce, &frame.nonce);
                self.snonce = nonce;
                self.anonce = frame.nonce;
                self.ptk = Some(material);
                self.m1 = Some(frame.replay);
                self.last_replay = Some(frame.replay);
                self.accepted = None;
            }
            let info = 0x10a | if self.authorized { 0x200 } else { 0 };
            let kck = &self.ptk.as_ref().unwrap()[..16];
            return Ok(Action::Reply(response(
                frame.raw[0],
                info,
                frame.replay,
                &self.snonce,
                &RSN_IE,
                kck,
            )));
        }
        let material = self.ptk.as_ref().ok_or(Errno::EACCES)?;
        if frame.info & 0x100 == 0 {
            return Err(Errno::EACCES);
        }
        frame.verify(&material[..16])?;
        if let Some((replay, mic, was_pairwise)) = self.accepted {
            if frame.replay == replay && frame.raw[81..97] == mic && was_pairwise == pairwise {
                if self.pending {
                    return Err(Errno::EALREADY);
                }
                return Ok(Action::Reply(response(
                    frame.raw[0],
                    if pairwise { 0x30a } else { 0x302 },
                    replay,
                    &[0; 32],
                    &[],
                    &material[..16],
                )));
            }
        }
        if self.last_replay.is_some_and(|old| frame.replay <= old) || self.pending {
            return Err(Errno::EACCES);
        }
        if pairwise
            && (self.accepted.is_some()
                || self.anonce != frame.nonce
                || frame.info & (0x40 | 0x200) != 0x240)
        {
            return Err(Errno::EACCES);
        }
        if !pairwise && (!self.authorized || frame.info & (0x40 | 0x200 | 0x1000) != 0x1200) {
            return Err(Errno::EACCES);
        }
        let plain = key_data(&frame, &material[16..32])?;
        let mut group = gtk(&plain, frame.rsc, pairwise, frame.info & 0x1000 != 0)?;
        if !pairwise && group.is_none() {
            return Err(Errno::EACCES);
        }
        if let Some(key) = &group {
            if let Some((index, value)) = &self.group {
                if **value == *key.key {
                    if *index != key.index {
                        return Err(Errno::EACCES);
                    }
                    group = None;
                }
            }
        }
        let pairwise_key = if pairwise {
            Some(Zeroizing::new(material[32..48].try_into().unwrap()))
        } else {
            None
        };
        let reply = response(
            frame.raw[0],
            if pairwise { 0x30a } else { 0x302 },
            frame.replay,
            &[0; 32],
            &[],
            &material[..16],
        );
        self.last_replay = Some(frame.replay);
        self.accepted = Some((
            frame.replay,
            frame.raw[81..97].try_into().unwrap(),
            pairwise,
        ));
        if let Some(key) = &group {
            self.group = Some((key.index, Zeroizing::new(*key.key)));
        }
        if pairwise_key.is_none() && group.is_none() {
            return Ok(Action::Reply(reply));
        }
        self.pending = true;
        Ok(Action::Install {
            keys: Keys {
                pairwise: pairwise_key,
                group,
            },
            reply,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hidden_input_drains_overlong_or_invalid_passwords_without_exposing_a_suffix() {
        for prefix in [alloc::vec![b'x';65], b"secret\x01".to_vec()] {
            let mut reader = PasswordReader::new();
            for byte in prefix.iter().chain(b"private suffix").copied() {
                assert!(reader.receive(byte).is_none());
            }
            assert!(matches!(reader.receive(b'\n'), Some(Err(Errno::EINVAL))));
        }
        let mut reader = PasswordReader::new();
        for &byte in b"passwordx\x08" {
            assert!(reader.receive(byte).is_none());
        }
        assert_eq!(&*reader.receive(b'\n').unwrap().unwrap(), b"password");
        let mut reader = PasswordReader::new();
        reader.receive(b'x');
        assert!(matches!(reader.receive(3), Some(Err(Errno::EINTR))));
    }
    fn hex(text: &str) -> Vec<u8> {
        text.as_bytes()
            .chunks_exact(2)
            .map(|b| u8::from_str_radix(core::str::from_utf8(b).unwrap(), 16).unwrap())
            .collect()
    }
    const STATION: [u8; 6] = [2, 3, 4, 5, 6, 8];
    const AP: [u8; 6] = [2, 3, 4, 5, 6, 7];
    fn material() -> Zeroizing<[u8; 48]> {
        ptk(
            &Zeroizing::new([0x44; 32]),
            STATION,
            AP,
            &[0x22; 32],
            &[0x11; 32],
        )
    }
    fn ap_frame(info: u16, replay: u64, nonce: [u8; 32], data: &[u8], kck: &[u8]) -> Vec<u8> {
        let mut frame = response(2, info, replay, &nonce, data, kck);
        frame[7..9].copy_from_slice(&16u16.to_be_bytes());
        frame[81..97].fill(0);
        if info & 0x100 != 0 {
            let mut mic = HmacSha1::new_from_slice(kck).unwrap();
            mic.update(&frame);
            frame[81..97].copy_from_slice(&mic.finalize().into_bytes()[..16]);
        }
        frame
    }
    fn handshake() -> Handshake {
        Handshake::new(Zeroizing::new([0x44; 32]), STATION, AP).unwrap()
    }
    fn gtk_plain(value: u8, index: u8, with_rsn: bool) -> Vec<u8> {
        let mut data = if with_rsn {
            RSN_IE.to_vec()
        } else {
            Vec::new()
        };
        data.extend_from_slice(&[0xdd, 22, 0, 0x0f, 0xac, 1, index, 0]);
        data.extend_from_slice(&[value; 16]);
        if data.len() % 8 != 0 {
            data.push(0xdd);
            while data.len() % 8 != 0 {
                data.push(0);
            }
        }
        data
    }
    fn encrypted(value: u8, index: u8, with_rsn: bool) -> Vec<u8> {
        let material = material();
        let key: &[u8; 16] = material[16..32].try_into().unwrap();
        aes_kw::Kek::<aes::Aes128>::new(key.into())
            .wrap_vec(&gtk_plain(value, index, with_rsn))
            .unwrap()
    }
    fn first(handshake: &mut Handshake) -> Vec<u8> {
        let m1 = ap_frame(0x8a, 1, [0x11; 32], &[], &[0; 16]);
        match handshake
            .receive(&m1, |out| {
                out.fill(0x22);
                Ok(())
            })
            .unwrap()
        {
            Action::Reply(frame) => frame,
            _ => panic!("M1 must not install keys"),
        }
    }
    fn m3() -> Vec<u8> {
        ap_frame(
            0x13ca,
            2,
            [0x11; 32],
            &encrypted(0x55, 1, true),
            &material()[..16],
        )
    }
    #[test]
    fn ieee_pmk_and_independent_prf_fixtures_match_every_key_byte() {
        assert_eq!(
            &derive(b"password", b"IEEE").unwrap()[..],
            hex("f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e")
        );
        assert_eq!(
            &material()[..],
            hex(
                "f9809bea25c2c6c5a0db9546ff9c9255533b8bf2e80348e5a4defd57c64f239a77854580970da7273746f7251b3bdc51"
            )
        );
        assert!(derive(b"short", b"ssid").is_err());
        assert!(derive(b"password", b"").is_err());
        assert!(derive(&[b'x'; 64], b"ssid").is_err());
        let raw = b"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
        assert_eq!(
            &derive(raw, b"ssid").unwrap()[..],
            &(0u8..32).collect::<Vec<_>>()
        );
    }
    #[test]
    fn rfc3394_key_unwrap_vector_and_integrity_rejection() {
        let wrapped = hex("1fa68b0a8112b447aef34bd8fb5a7b829d3e862371d2cfe5");
        let key: [u8; 16] = core::array::from_fn(|i| i as u8);
        let frame = KeyFrame {
            raw: &[],
            info: 0x1000,
            replay: 0,
            nonce: [0; 32],
            data: &wrapped,
            rsc: [0; 8],
        };
        assert_eq!(
            &key_data(&frame, &key).unwrap()[..],
            hex("00112233445566778899aabbccddeeff")
        );
        let mut corrupt = wrapped.clone();
        corrupt[0] ^= 1;
        assert!(
            key_data(
                &KeyFrame {
                    data: &corrupt,
                    ..frame
                },
                &key
            )
            .is_err()
        );
    }
    #[test]
    fn pairwise_handshake_authorizes_only_after_install_and_never_reinstalls_on_m3_retry() {
        let mut handshake = handshake();
        let m2 = first(&mut handshake);
        assert_eq!(&m2[5..9], &[1, 10, 0, 0]);
        assert_eq!(&m2[17..49], &[0x22; 32]);
        assert_eq!(&m2[99..], &RSN_IE);
        assert!(!handshake.authorized());
        let frame = m3();
        match handshake
            .receive(&frame, |_| panic!("M3 must not obtain a nonce"))
            .unwrap()
        {
            Action::Install { keys, reply } => {
                assert_eq!(&**keys.pairwise.as_ref().unwrap(), &material()[32..48]);
                assert_eq!(&keys.group.as_ref().unwrap().key[..], &[0x55; 16]);
                assert_eq!(&reply[5..9], &[3, 10, 0, 0]);
                assert!(reply[17..49].iter().all(|&b| b == 0));
            }
            _ => panic!("verified M3 must request key installation"),
        }
        assert!(!handshake.authorized());
        assert!(matches!(
            handshake.receive(&frame, |_| unreachable!()),
            Err(Errno::EALREADY)
        ));
        handshake.installed().unwrap();
        assert!(handshake.authorized());
        assert!(matches!(
            handshake.receive(&frame, |_| unreachable!()).unwrap(),
            Action::Reply(_)
        ));
        assert_eq!(handshake.installed(), Err(Errno::EALREADY));
    }
    #[test]
    fn bad_mic_nonce_replay_or_entropy_never_installs_keys() {
        let mut handshake = handshake();
        let m1 = ap_frame(0x8a, 1, [0x11; 32], &[], &[0; 16]);
        assert!(matches!(
            handshake.receive(&m1, |_| Err(Errno::EOPNOTSUPP)),
            Err(Errno::EOPNOTSUPP)
        ));
        let m2 = first(&mut handshake);
        match handshake
            .receive(&m1, |_| panic!("repeated M1 reuses its SNonce"))
            .unwrap()
        {
            Action::Reply(repeated) => assert_eq!(m2, repeated),
            _ => panic!("M1 retry"),
        }
        let mut bad = m3();
        bad[81] ^= 1;
        assert!(matches!(
            handshake.receive(&bad, |_| unreachable!()),
            Err(Errno::EACCES)
        ));
        let bad = ap_frame(
            0x13ca,
            2,
            [0x33; 32],
            &encrypted(0x55, 1, true),
            &material()[..16],
        );
        assert!(matches!(
            handshake.receive(&bad, |_| unreachable!()),
            Err(Errno::EACCES)
        ));
        let bad = ap_frame(
            0x13ca,
            1,
            [0x11; 32],
            &encrypted(0x55, 1, true),
            &material()[..16],
        );
        assert!(matches!(
            handshake.receive(&bad, |_| unreachable!()),
            Err(Errno::EACCES)
        ));
        assert_eq!(handshake.installed(), Err(Errno::EALREADY));
        assert!(!handshake.authorized());
        for end in 0..m1.len() {
            assert!(KeyFrame::parse(&m1[..end]).is_err());
        }
    }
    #[test]
    fn group_rekey_and_duplicates_do_not_reset_an_installed_group_key() {
        let mut handshake = handshake();
        first(&mut handshake);
        assert!(matches!(
            handshake.receive(&m3(), |_| unreachable!()).unwrap(),
            Action::Install { .. }
        ));
        handshake.installed().unwrap();
        let rekey = ap_frame(
            0x1382,
            3,
            [0; 32],
            &encrypted(0x66, 2, false),
            &material()[..16],
        );
        match handshake.receive(&rekey, |_| unreachable!()).unwrap() {
            Action::Install { keys, reply } => {
                assert!(keys.pairwise.is_none());
                assert_eq!(keys.group.unwrap().index, 2);
                assert_eq!(&reply[5..7], &[3, 2]);
            }
            _ => panic!("new GTK must be installed once"),
        }
        handshake.installed().unwrap();
        assert!(matches!(
            handshake.receive(&rekey, |_| unreachable!()).unwrap(),
            Action::Reply(_)
        ));
        let same_key = ap_frame(
            0x1382,
            4,
            [0; 32],
            &encrypted(0x66, 2, false),
            &material()[..16],
        );
        assert!(matches!(
            handshake.receive(&same_key, |_| unreachable!()).unwrap(),
            Action::Reply(_)
        ));
        let moved_key = ap_frame(
            0x1382,
            5,
            [0; 32],
            &encrypted(0x66, 1, false),
            &material()[..16],
        );
        assert!(matches!(
            handshake.receive(&moved_key, |_| unreachable!()),
            Err(Errno::EACCES)
        ));
    }
}
