//! Block devices: storage that is read and written in whole blocks
//!
//! File systems only see `BlockDevice`. Today the only device is the ramdisk
//! (`MemBlockDevice`); a disk driver later implements the same trait.

use crate::errno::{Errno, KResult};
use alloc::vec;

/// Block size of `MemBlockDevice`, the sector size of most disks
pub const SECTOR_SIZE: usize = 512;

/// Storage addressed by block numbers (LBA)
pub trait BlockDevice: Send + Sync {
    /// Bytes in one block, a power of two
    fn block_size(&self) -> usize;

    /// Number of blocks on the device
    fn num_blocks(&self) -> u64;

    /// Reads whole blocks starting at block `lba`. `buf.len()` must be a multiple of
    /// the block size, otherwise `EINVAL`; reading past the end is `EIO`.
    fn read_blocks(&self, lba: u64, buf: &mut [u8]) -> KResult<()>;

    /// Writes whole blocks starting at block `lba`, read-only devices answer `EROFS`
    fn write_blocks(&self, _lba: u64, _buf: &[u8]) -> KResult<()> {
        Err(Errno::EROFS)
    }
}

/// Reads `buf.len()` bytes at byte `offset`, which need not be block aligned.
/// Reading past the end of the device is `EIO`.
pub fn read_bytes(device: &dyn BlockDevice, offset: u64, buf: &mut [u8]) -> KResult<()> {
    let block_size = device.block_size() as u64;
    let end = offset.checked_add(buf.len() as u64).ok_or(Errno::EIO)?;
    let capacity = device.num_blocks().checked_mul(block_size).ok_or(Errno::EIO)?;
    if end > capacity {
        return Err(Errno::EIO);
    }
    let mut block = vec![0u8; block_size as usize];
    let mut done = 0;
    while done < buf.len() {
        let position = offset + done as u64;
        let lba = position / block_size;
        let within = (position % block_size) as usize;
        let chunk = (block_size as usize - within).min(buf.len() - done);
        device.read_blocks(lba, &mut block)?;
        buf[done..done + chunk].copy_from_slice(&block[within..within + chunk]);
        done += chunk;
    }
    Ok(())
}

/// A read-only device over bytes in memory, e.g. the ramdisk the bootloader loaded.
/// A partial last block reads as if padded with zeros.
pub struct MemBlockDevice {
    data: &'static [u8],
}

impl MemBlockDevice {
    pub const fn new(data: &'static [u8]) -> Self {
        Self { data }
    }

    /// The bytes behind the device
    pub fn data(&self) -> &'static [u8] {
        self.data
    }
}

impl BlockDevice for MemBlockDevice {
    fn block_size(&self) -> usize {
        SECTOR_SIZE
    }

    fn num_blocks(&self) -> u64 {
        self.data.len().div_ceil(SECTOR_SIZE) as u64
    }

    fn read_blocks(&self, lba: u64, buf: &mut [u8]) -> KResult<()> {
        if buf.len() % SECTOR_SIZE != 0 {
            return Err(Errno::EINVAL);
        }
        let blocks = (buf.len() / SECTOR_SIZE) as u64;
        if lba.checked_add(blocks).is_none_or(|end| end > self.num_blocks()) {
            return Err(Errno::EIO);
        }
        let start = lba as usize * SECTOR_SIZE;
        let available = self.data.len().saturating_sub(start).min(buf.len());
        buf[..available].copy_from_slice(&self.data[start..start + available]);
        buf[available..].fill(0);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    fn device(len: usize) -> MemBlockDevice {
        let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        MemBlockDevice::new(data.leak())
    }

    #[test]
    fn partial_last_block_is_padded_with_zeros() {
        let device = device(SECTOR_SIZE + 10);
        assert_eq!(device.num_blocks(), 2);
        let mut block = [0xffu8; SECTOR_SIZE];
        device.read_blocks(1, &mut block).unwrap();
        assert_eq!(&block[..10], &device.data()[SECTOR_SIZE..]);
        assert!(block[10..].iter().all(|&b| b == 0));
    }

    #[test]
    fn empty_device_has_no_blocks() {
        let device = device(0);
        assert_eq!(device.num_blocks(), 0);
        assert_eq!(device.read_blocks(0, &mut [0; SECTOR_SIZE]), Err(Errno::EIO));
        assert_eq!(read_bytes(&device, 0, &mut []), Ok(()));
    }

    #[test]
    fn whole_blocks_only() {
        let device = device(4 * SECTOR_SIZE);
        assert_eq!(device.read_blocks(0, &mut [0; 100]), Err(Errno::EINVAL));
        let mut two = [0u8; 2 * SECTOR_SIZE];
        device.read_blocks(1, &mut two).unwrap();
        assert_eq!(&two[..], &device.data()[SECTOR_SIZE..3 * SECTOR_SIZE]);
    }

    #[test]
    fn reads_past_the_end_fail() {
        let device = device(4 * SECTOR_SIZE);
        assert_eq!(device.read_blocks(4, &mut [0; SECTOR_SIZE]), Err(Errno::EIO));
        assert_eq!(device.read_blocks(3, &mut [0; 2 * SECTOR_SIZE]), Err(Errno::EIO));
        assert_eq!(device.read_blocks(u64::MAX, &mut [0; SECTOR_SIZE]), Err(Errno::EIO));
    }

    #[test]
    fn writes_are_refused() {
        assert_eq!(device(SECTOR_SIZE).write_blocks(0, &[0; SECTOR_SIZE]), Err(Errno::EROFS));
    }

    #[test]
    fn read_bytes_at_every_offset_and_length() {
        let device = device(3 * SECTOR_SIZE + 7);
        let data = device.data();
        let mut buf = [0u8; 2 * SECTOR_SIZE];
        // around both block boundaries and the end of the data
        let points = [0, 1, 511, 512, 513, 1023, 1024, 1025, 1535, 1536, 1540, 1543];
        for &offset in &points {
            for &len in &[0, 1, 2, 511, 512, 513, 1024] {
                let result = read_bytes(&device, offset as u64, &mut buf[..len]);
                if offset + len <= 4 * SECTOR_SIZE {
                    result.unwrap_or_else(|e| panic!("offset {offset} len {len}: {e}"));
                    let real = data.len().saturating_sub(offset).min(len);
                    assert_eq!(&buf[..real], &data[offset..offset + real], "offset {offset} len {len}");
                    assert!(buf[real..len].iter().all(|&b| b == 0), "offset {offset} len {len}");
                } else {
                    assert_eq!(result, Err(Errno::EIO), "offset {offset} len {len}");
                }
            }
        }
    }

    #[test]
    fn read_bytes_offset_overflow() {
        assert_eq!(read_bytes(&device(SECTOR_SIZE), u64::MAX, &mut [0; 2]), Err(Errno::EIO));
    }

    /// A device with 4 KiB blocks, file systems must not assume 512
    struct BigBlocks(MemBlockDevice);

    impl BlockDevice for BigBlocks {
        fn block_size(&self) -> usize {
            4096
        }
        fn num_blocks(&self) -> u64 {
            self.0.data().len().div_ceil(4096) as u64
        }
        fn read_blocks(&self, lba: u64, buf: &mut [u8]) -> KResult<()> {
            self.0.read_blocks(lba * 8, buf)
        }
    }

    #[test]
    fn read_bytes_with_other_block_size() {
        let device = BigBlocks(device(3 * 4096));
        let mut buf = [0u8; 5000];
        read_bytes(&device, 4000, &mut buf).unwrap();
        assert_eq!(&buf[..], &device.0.data()[4000..9000]);
    }
}
