// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, ensure, Error};
use zerocopy::{AsBytes, FromBytes, FromZeroes, NoCell};

const MAX_PARTITION_ENTRIES: u32 = 128;
const GPT_HEADER_SIZE: usize = 92;

/// GPT disk header.
#[derive(Clone, Debug, Eq, PartialEq, NoCell, AsBytes, FromZeroes, FromBytes)]
#[repr(C)]
pub struct Header {
    /// Must be "EFI PART"
    pub signature: [u8; 8],
    /// Must be 0x10000
    pub revision: u32,
    pub header_size: u32,
    /// CRC32 of the header with crc32 section zeroed
    pub crc32: u32,
    /// reserved; must be 0
    pub reserved: u32,
    /// Must be 1
    pub current_lba: u64,
    /// LBA of backup header
    pub backup_lba: u64,
    /// First usable LBA for partitions (primary table last LBA + 1)
    pub first_usable: u64,
    /// Last usable LBA (secondary partition table first LBA - 1)
    pub last_usable: u64,
    /// UUID of the disk
    pub disk_guid: [u8; 16],
    /// Starting LBA of partition entries
    pub part_start: u64,
    /// Number of partition entries
    pub num_parts: u32,
    /// Size of a partition entry, usually 128
    pub part_size: u32,
    /// CRC32 of the partition table
    pub crc32_parts: u32,
    /// Padding to satisfy zerocopy's alignment requirements.
    /// Not actually part of the header, which should be GPT_HEADER_SIZE bytes.
    zerocopy_padding: u32,
}

impl Header {
    fn compute_checksum(&self) -> u32 {
        let mut header_copy = self.clone();
        header_copy.crc32 = 0;
        crc::crc32::checksum_ieee(&header_copy.as_bytes()[..GPT_HEADER_SIZE])
    }

    // NB: This does *not* validate the partition table checksum.
    pub fn ensure_integrity(&self, block_count: u64, block_size: u64) -> Result<(), Error> {
        ensure!(
            self.signature == [0x45, 0x46, 0x49, 0x20, 0x50, 0x41, 0x52, 0x54],
            "Bad signature {:x?}",
            self.signature
        );
        ensure!(self.revision == 0x10000, "Bad revision {:x}", self.revision);
        ensure!(
            self.header_size as usize == GPT_HEADER_SIZE,
            "Bad header size {}",
            self.header_size
        );

        // Now that we've sanity-checked the basic fields, check the CRC.  All other checks should
        // be below this.
        ensure!(self.crc32 == self.compute_checksum(), "Invalid header checksum");

        ensure!(self.num_parts <= MAX_PARTITION_ENTRIES, "Invalid num_parts {}", self.num_parts);
        ensure!(
            self.part_size as usize == std::mem::size_of::<PartitionTableEntry>(),
            "Invalid part_size {}",
            self.part_size
        );
        let partition_table_blocks = (self
            .num_parts
            .checked_mul(self.part_size)
            .and_then(|v| v.checked_next_multiple_of(block_size as u32))
            .ok_or(anyhow!("Partition table size overflow"))?
            as u64)
            / block_size;
        ensure!(partition_table_blocks < block_count, "Invalid partition table size");

        // NB: The current LBA points to *this* header, so it's either at the start or the end.
        // The last LBA points to the *other* header.  Since we want to check the absolute offsets,
        // figure out which is which.
        ensure!(
            self.current_lba == 1 || self.current_lba == block_count - 1,
            "Invalid current_lba {}",
            self.current_lba
        );
        let (first_lba, second_lba) = if self.current_lba == 1 {
            (self.current_lba, self.backup_lba)
        } else {
            (self.backup_lba, self.current_lba)
        };

        ensure!(
            self.first_usable >= first_lba + partition_table_blocks,
            "Invalid first_usable {}",
            self.first_usable
        );
        ensure!(
            self.first_usable < self.last_usable
                && self.last_usable + partition_table_blocks <= second_lba,
            "Invalid last_usable {}",
            self.last_usable
        );

        if first_lba == self.current_lba {
            ensure!(self.part_start == first_lba + 1, "Invalid part_start {}", self.part_start);
        } else {
            ensure!(
                self.part_start == self.last_usable + 1,
                "Invalid part_start {}",
                self.part_start
            );
        }

        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, NoCell, AsBytes, FromZeroes, FromBytes)]
#[repr(C)]
pub struct PartitionTableEntry {
    pub type_guid: [u8; 16],
    pub instance_guid: [u8; 16],
    pub first_lba: u64,
    pub last_lba: u64,
    pub flags: u64,
    pub name: [u16; 36],
}

impl PartitionTableEntry {
    pub fn is_empty(&self) -> bool {
        self.as_bytes().iter().all(|b| *b == 0)
    }

    pub fn ensure_integrity(&self) -> Result<(), Error> {
        ensure!(self.type_guid != [0u8; 16], "Empty type GUID");
        ensure!(self.instance_guid != [0u8; 16], "Empty instance GUID");
        ensure!(self.first_lba != 0, "Invalid first LBA");
        ensure!(self.last_lba != 0 && self.last_lba >= self.first_lba, "Invalid last LBA");
        Ok(())
    }
}

#[cfg(test)]
pub mod testing {
    use super::{Header, PartitionTableEntry, GPT_HEADER_SIZE};
    use crc::Hasher32 as _;
    use fuchsia_zircon as zx;
    use zerocopy::AsBytes as _;

    // Used for [`format_gpt`].
    pub struct PartitionDescriptor {
        pub label: String,
        pub type_guid: uuid::Uuid,
        pub instance_guid: uuid::Uuid,
        pub start_block: u64,
        pub num_blocks: u64,
    }

    impl PartitionDescriptor {
        fn as_entry(&self) -> PartitionTableEntry {
            let mut name = [0u16; 36];
            let raw = self.label.encode_utf16().collect::<Vec<_>>();
            assert!(raw.len() <= name.len());
            name[..raw.len()].copy_from_slice(&raw[..]);
            PartitionTableEntry {
                type_guid: self.type_guid.into_bytes(),
                instance_guid: self.instance_guid.into_bytes(),
                first_lba: self.start_block,
                last_lba: self.start_block + self.num_blocks,
                flags: 0,
                name,
            }
        }
    }

    /// Formats `vmo` with the given partition table.  Assumes the VMO is big enough for all
    /// partitions to be at a valid offset.
    pub fn format_gpt(vmo: &zx::Vmo, block_size: u32, partitions: Vec<PartitionDescriptor>) {
        // TODO(https://fxbug.dev/339491886): We should write the PMBR.

        assert!(block_size > 0);
        let bs = block_size as u64;
        let len = vmo.get_size().unwrap() as usize;
        assert!(len % block_size as usize == 0);
        let nblocks = len as u64 / bs;

        let part_size = 128;
        let partition_table_len = partitions.len() * part_size;
        let partition_table_nblocks =
            partition_table_len.checked_next_multiple_of(block_size as usize).unwrap() as u64 / bs;

        // Ensure there are enough blocks for both copies of the metadata, plus the protective MBR
        // block.
        assert!(nblocks > 1 + 2 * (1 + partition_table_nblocks));

        let mut header = Header {
            signature: [0x45, 0x46, 0x49, 0x20, 0x50, 0x41, 0x52, 0x54],
            revision: 0x10000,
            header_size: GPT_HEADER_SIZE as u32,
            crc32: 0,
            reserved: 0,
            current_lba: 1,
            backup_lba: nblocks - 1,
            first_usable: 2 + partition_table_nblocks,
            last_usable: nblocks - (2 + partition_table_nblocks),
            disk_guid: [0u8; 16],
            part_start: 2,
            num_parts: partitions.len() as u32,
            part_size: part_size as u32,
            crc32_parts: 0,
            zerocopy_padding: 0,
        };

        let (partition_table, crc32_parts) = {
            let mut digest = crc::crc32::Digest::new(crc::crc32::IEEE);
            let mut partition_table = vec![0u8; partition_table_len as usize];
            let mut partition_table_view = &mut partition_table[..];
            for partition in &partitions {
                let part = partition.as_entry();
                assert!(part.first_lba >= header.first_usable);
                assert!(part.last_lba <= header.last_usable);
                let part_raw = part.as_bytes();
                assert!(part_raw.len() <= part_size);
                digest.write(part_raw);
                partition_table_view[..part_raw.len()].copy_from_slice(part_raw);
                partition_table_view = &mut partition_table_view[part_raw.len()..];
            }
            (partition_table, digest.sum32())
        };
        header.crc32_parts = crc32_parts;

        let write_header = |header: &Header, offset: u64| {
            let header_raw = header.as_bytes();
            vmo.write(header_raw, offset).unwrap();
        };
        let write_partition_table = |partition_table: &[u8], offset: u64| {
            vmo.write(partition_table, offset).unwrap();
        };

        header.crc32 = header.compute_checksum();
        write_header(&header, header.current_lba * bs);

        header.current_lba = header.backup_lba;
        header.backup_lba = 1;
        header.part_start = header.last_usable + 1;
        header.crc32 = header.compute_checksum();
        write_header(&header, header.current_lba * bs);

        write_partition_table(&partition_table[..], 2 * bs);
        write_partition_table(&partition_table[..], (nblocks - 1 - partition_table_nblocks) * bs);
    }
}

#[cfg(test)]
mod tests {
    use super::{Header, GPT_HEADER_SIZE};

    #[fuchsia::test]
    fn header_crc() {
        let nblocks = 8;
        let partition_table_nblocks = 1;
        let mut header = Header {
            signature: [0x45, 0x46, 0x49, 0x20, 0x50, 0x41, 0x52, 0x54],
            revision: 0x10000,
            header_size: GPT_HEADER_SIZE as u32,
            crc32: 0,
            reserved: 0,
            current_lba: 1,
            backup_lba: nblocks - 1,
            first_usable: 2 + partition_table_nblocks,
            last_usable: nblocks - (2 + partition_table_nblocks),
            disk_guid: [0u8; 16],
            part_start: 2,
            num_parts: 1,
            part_size: 128,
            crc32_parts: 0,
            zerocopy_padding: 0,
        };
        header.crc32 = header.compute_checksum();

        header.ensure_integrity(nblocks, 512).expect("Header should be valid");

        // Flip one bit, leaving an otherwise valid header.
        header.num_parts = 2;

        header.ensure_integrity(nblocks, 512).expect_err("Header should be invalid");
    }
}
