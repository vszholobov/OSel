//! Checked ath10k firmware/board containers. No register or DMA access.
pub mod bmi;
pub mod boot;
pub mod htc;
pub mod ieee80211;
pub mod wmi;
pub const ACER_BOARD: &[u8] =
    b"bus=pci,vendor=168c,device=003e,subsystem-vendor=11ad,subsystem-device=0807";
pub const FIRMWARE: &[u8] = include_bytes!("../../firmware/ath10k/QCA6174/hw3.0/firmware-6.bin");
pub const BOARDS: &[u8] = include_bytes!("../../firmware/ath10k/QCA6174/hw3.0/board-2.bin");
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Magic,
    Truncated,
    Duplicate,
    Missing,
    Version,
    Board,
}

struct Elements<'a>(&'a [u8]);
impl<'a> Elements<'a> {
    fn new(bytes: &'a [u8], magic: &[u8]) -> Result<Self, Error> {
        if !bytes.starts_with(magic) {
            return Err(Error::Magic);
        }
        let start = (magic.len() + 3) & !3;
        Ok(Self(bytes.get(start..).ok_or(Error::Truncated)?))
    }
    fn next(&mut self) -> Result<Option<(u32, &'a [u8])>, Error> {
        if self.0.is_empty() {
            return Ok(None);
        }
        let header = self.0.get(..8).ok_or(Error::Truncated)?;
        let id = u32::from_le_bytes(header[..4].try_into().unwrap());
        let len = u32::from_le_bytes(header[4..].try_into().unwrap()) as usize;
        let end = 8usize.checked_add(len).ok_or(Error::Truncated)?;
        let padded = end.checked_add(3).ok_or(Error::Truncated)? & !3;
        let value = self.0.get(8..end).ok_or(Error::Truncated)?;
        self.0 = self.0.get(padded..).ok_or(Error::Truncated)?;
        Ok(Some((id, value)))
    }
}

#[derive(Debug)]
pub struct Firmware<'a> {
    pub version: &'a str,
    pub image: &'a [u8],
    pub otp: &'a [u8],
    pub wmi: u32,
    pub htt: u32,
    pub ignore_otp_result: bool,
}
impl<'a> Firmware<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let mut tlvs = Elements::new(bytes, b"QCA-ATH10K\0")?;
        let mut version = None;
        let mut image = None;
        let mut otp = None;
        let mut wmi = None;
        let mut htt = None;
        let mut features = None;
        while let Some((id, data)) = tlvs.next()? {
            match id {
                0 => set(
                    &mut version,
                    core::str::from_utf8(data)
                        .map_err(|_| Error::Version)?
                        .trim_end_matches('\0'),
                )?,
                3 => set(&mut image, data)?,
                4 => set(&mut otp, data)?,
                5 => set(&mut wmi, word(data)?)?,
                6 => set(&mut htt, word(data)?)?,
                2 => set(&mut features, data)?,
                _ => {}
            }
        }
        let image = image.filter(|b| !b.is_empty()).ok_or(Error::Missing)?;
        Ok(Self {
            version: version.ok_or(Error::Missing)?,
            image,
            otp: otp.unwrap_or(&[]),
            wmi: wmi.ok_or(Error::Missing)?,
            htt: htt.ok_or(Error::Missing)?,
            ignore_otp_result: features
                .is_some_and(|data| data.first().is_some_and(|b| b & 0x80 != 0)),
        })
    }
}
fn word(data: &[u8]) -> Result<u32, Error> {
    Ok(u32::from_le_bytes(
        data.try_into().map_err(|_| Error::Truncated)?,
    ))
}
fn set<T>(slot: &mut Option<T>, value: T) -> Result<(), Error> {
    if slot.is_some() {
        return Err(Error::Duplicate);
    }
    *slot = Some(value);
    Ok(())
}

pub fn board<'a>(bytes: &'a [u8], name: &[u8]) -> Result<&'a [u8], Error> {
    let mut tlvs = Elements::new(bytes, b"QCA-ATH10K-BOARD\0")?;
    let mut found = None;
    while let Some((id, data)) = tlvs.next()? {
        if id != 0 {
            continue;
        }
        let mut entry = Elements(data);
        let mut matches = false;
        while let Some((id, value)) = entry.next()? {
            match id {
                0 => matches = value == name,
                1 if matches => {
                    if value.len() != 8124 {
                        return Err(Error::Board);
                    }
                    set(&mut found, value)?;
                    matches = false;
                }
                _ => {}
            }
        }
    }
    found.ok_or(Error::Missing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{vec, vec::Vec};
    fn tlv(dst: &mut Vec<u8>, id: u32, data: &[u8]) {
        while dst.len() % 4 != 0 {
            dst.push(0);
        }
        dst.extend(id.to_le_bytes());
        dst.extend((data.len() as u32).to_le_bytes());
        dst.extend(data);
        while dst.len() % 4 != 0 {
            dst.push(0);
        }
    }
    fn fw() -> Vec<u8> {
        let mut bytes = b"QCA-ATH10K\0\0".to_vec();
        tlv(&mut bytes, 0, b"test");
        tlv(&mut bytes, 3, b"image");
        tlv(&mut bytes, 5, &4u32.to_le_bytes());
        tlv(&mut bytes, 6, &3u32.to_le_bytes());
        bytes
    }
    #[test]
    fn rejects_truncation_overflow_duplicates_and_bad_magic() {
        let good = fw();
        assert_eq!(Firmware::parse(&good).unwrap().image, b"image");
        for end in 0..good.len() {
            assert!(Firmware::parse(&good[..end]).is_err());
        }
        let mut bad = good.clone();
        bad[0] = 0;
        assert!(matches!(Firmware::parse(&bad), Err(Error::Magic)));
        let mut bad = good.clone();
        bad[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Firmware::parse(&bad).is_err());
        let mut bad = good;
        tlv(&mut bad, 3, b"other");
        assert!(matches!(Firmware::parse(&bad), Err(Error::Duplicate)));
    }
    #[test]
    fn requires_exact_board_and_checked_nested_lengths() {
        let mut bytes = b"QCA-ATH10K-BOARD\0".to_vec();
        let mut entry = Vec::new();
        tlv(&mut entry, 0, b"other");
        tlv(&mut entry, 1, &vec![1; 8124]);
        tlv(&mut entry, 0, ACER_BOARD);
        tlv(&mut entry, 1, &vec![2; 8124]);
        tlv(&mut bytes, 0, &entry);
        assert_eq!(board(&bytes, ACER_BOARD).unwrap(), &vec![2; 8124]);
        assert_eq!(board(&bytes, b"wrong"), Err(Error::Missing));
        for end in 0..bytes.len() {
            assert!(board(&bytes[..end], ACER_BOARD).is_err());
        }
    }
    #[test]
    fn embedded_acer_firmware_has_exact_board_and_tlv_protocols() {
        let fw = Firmware::parse(FIRMWARE).unwrap();
        assert_eq!(fw.wmi, 4);
        assert_eq!(fw.htt, 3);
        assert!(!fw.image.is_empty());
        assert!(!fw.otp.is_empty());
        assert_eq!(board(BOARDS, ACER_BOARD).unwrap().len(), 8124);
    }
}
