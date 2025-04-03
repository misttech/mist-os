// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::superblock::{f2fs_crc32, BLOCK_SIZE, F2FS_MAGIC, SEGMENT_SIZE};
use anyhow::{anyhow, ensure, Error};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

const MAX_ACTIVE_NODE_LOGS: usize = 8;
const MAX_ACTIVE_DATA_LOGS: usize = 8;
const MAX_ACTIVE_LOGS: usize = 16;
pub const CKPT_FLAG_UNMOUNT: u32 = 0x1;
pub const CKPT_FLAG_COMPACT_SUMMARY: u32 = 0x4;

#[derive(Debug, Eq, PartialEq, FromBytes, Immutable, IntoBytes, KnownLayout)]
#[repr(C, packed)]
pub struct CheckpointHeader {
    pub checkpoint_ver: u64,
    pub user_block_count: u64,
    pub valid_block_count: u64,
    pub rsvd_segment_count: u32,
    pub overprov_segment_count: u32,
    pub free_segment_count: u32,
    pub cur_node_segno: [u32; MAX_ACTIVE_NODE_LOGS],
    pub cur_node_blkoff: [u16; MAX_ACTIVE_NODE_LOGS],
    pub cur_data_segno: [u32; MAX_ACTIVE_DATA_LOGS],
    pub cur_data_blkoff: [u16; MAX_ACTIVE_DATA_LOGS],
    pub ckpt_flags: u32,
    pub cp_pack_total_block_count: u32,
    pub cp_pack_start_sum: u32,
    pub valid_node_count: u32,
    pub valid_inode_count: u32,
    pub next_free_nid: u32,
    pub sit_ver_bitmap_bytesize: u32,
    pub nat_ver_bitmap_bytesize: u32,
    pub checksum_offset: u32,
    pub elapsed_time: u64,
    pub alloc_type: [u8; MAX_ACTIVE_LOGS],
    // SIT bitmap follows.
    // NAT bitmap follows.
}

#[derive(Debug)]
pub struct CheckpointPack {
    pub header: CheckpointHeader,
    pub nat_bitmap: Vec<u8>,
}

impl CheckpointPack {
    pub async fn read_from_device(
        device: &dyn storage_device::Device,
        offset: u64,
    ) -> Result<Self, Error> {
        let mut segment = device.allocate_buffer(SEGMENT_SIZE).await;
        device.read(offset, segment.as_mut()).await?;
        let segment = segment.as_slice();
        Self::parse_checkpoint(segment)
    }

    fn parse_checkpoint(segment: &[u8]) -> Result<Self, Error> {
        ensure!(
            segment.len() >= std::mem::size_of::<CheckpointHeader>(),
            "Segment too short for checkpoint"
        );
        let header =
            CheckpointHeader::read_from_bytes(&segment[..std::mem::size_of::<CheckpointHeader>()])
                .map_err(|_| anyhow!("Invalid checkpoint header"))?;
        let mut checksum: u32 = 0;
        let len = header.checksum_offset as usize;
        ensure!(len <= segment.len() - std::mem::size_of::<u32>(), "Bad checkpoint offset");
        checksum.as_mut_bytes().copy_from_slice(&segment[len..len + std::mem::size_of::<u32>()]);
        let crc32 = f2fs_crc32(F2FS_MAGIC, &segment[..len]);
        ensure!(crc32 == checksum, "Bad Checkpoint checksum ({crc32:08x} != {checksum:08x})");
        let sit_bitmap_start = std::mem::size_of::<CheckpointHeader>();
        let nat_bitmap_start = sit_bitmap_start + header.sit_ver_bitmap_bytesize as usize;
        let nat_bitmap_end = nat_bitmap_start + header.nat_ver_bitmap_bytesize as usize;
        ensure!(sit_bitmap_start < nat_bitmap_start, "Invalid sit_bitmap size");
        ensure!(nat_bitmap_start < nat_bitmap_end, "Invalid nat_bitmap size");
        ensure!(nat_bitmap_end <= SEGMENT_SIZE, "Invalid nat_bitmap range");
        let nat_bitmap = segment[nat_bitmap_start..nat_bitmap_end].to_vec();

        let backup_header_offset =
            (header.cp_pack_total_block_count as usize - 1) * BLOCK_SIZE as usize;
        ensure!(
            backup_header_offset + std::mem::size_of::<CheckpointHeader>() < SEGMENT_SIZE,
            "Invalid cp_pack_total_block_count"
        );
        let backup_header = CheckpointHeader::read_from_bytes(
            &segment[backup_header_offset
                ..backup_header_offset + std::mem::size_of::<CheckpointHeader>()],
        )
        .map_err(|_| anyhow!("Invalid backup header"))?;
        // If the backup copy is bad, fail this checkpoint (same as f2fs Fuchsia).
        ensure!(backup_header == header, "CheckpointHeader and backup differ");
        Ok(Self { header, nat_bitmap })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Some basic robustness coverage.
    #[test]
    fn test_checkpoint_parsing() {
        assert!(CheckpointPack::parse_checkpoint(&[]).is_err());

        let mut segment = Vec::new();
        segment.resize(SEGMENT_SIZE, 0);
        let mut header =
            CheckpointHeader::read_from_bytes(&segment[..std::mem::size_of::<CheckpointHeader>()])
                .unwrap();

        // helper to copy header into segment and set the checksum to a valid value.
        let set_header = |segment: &mut [u8], header: &CheckpointHeader| {
            segment[..std::mem::size_of::<CheckpointHeader>()].copy_from_slice(header.as_bytes());
            let crc32 = f2fs_crc32(F2FS_MAGIC, &segment[..header.checksum_offset as usize]);
            segment[header.checksum_offset as usize..header.checksum_offset as usize + 4]
                .copy_from_slice(crc32.as_bytes());
        };

        // Bad checksum offset.
        {
            header.checksum_offset = SEGMENT_SIZE as u32 - 3;
            segment[..std::mem::size_of::<CheckpointHeader>()].copy_from_slice(header.as_bytes());
            assert!(CheckpointPack::parse_checkpoint(&segment).is_err());
        }
        // Bad SIT size.
        {
            header.checksum_offset = std::mem::size_of::<CheckpointHeader>() as u32;
            header.sit_ver_bitmap_bytesize = SEGMENT_SIZE as u32;
            set_header(&mut segment, &header);
            assert!(CheckpointPack::parse_checkpoint(&segment).is_err());
        }
        // Bad NAT size.
        {
            header.sit_ver_bitmap_bytesize = 0;
            header.nat_ver_bitmap_bytesize = SEGMENT_SIZE as u32;
            set_header(&mut segment, &header);
            assert!(CheckpointPack::parse_checkpoint(&segment).is_err());
        }
        // Bad SIT+NAT size.
        {
            header.sit_ver_bitmap_bytesize = SEGMENT_SIZE as u32 / 2;
            header.nat_ver_bitmap_bytesize = SEGMENT_SIZE as u32 / 2;
            set_header(&mut segment, &header);
            assert!(CheckpointPack::parse_checkpoint(&segment).is_err());
        }
        // Bad cp_pack_total_block_count (more than one segment).
        {
            header.sit_ver_bitmap_bytesize = SEGMENT_SIZE as u32 / 4;
            header.nat_ver_bitmap_bytesize = SEGMENT_SIZE as u32 / 4;
            header.cp_pack_total_block_count = 2048;
            set_header(&mut segment, &header);
            assert!(CheckpointPack::parse_checkpoint(&segment).is_err());
        }
        // Different backup checkpoint.
        {
            header.cp_pack_total_block_count = 100;
            set_header(&mut segment, &header);
            assert!(CheckpointPack::parse_checkpoint(&segment).is_err());
        }
        // Success.
        {
            segment.copy_within(..std::mem::size_of::<CheckpointHeader>(), BLOCK_SIZE * 99);
            let result = CheckpointPack::parse_checkpoint(&segment);
            assert!(result.is_ok(), "{:?}", result);
        }
    }
}
