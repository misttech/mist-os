// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{bail, Error};
use zerocopy::{AsBytes, FromBytes, FromZeroes, NoCell};

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
    /// Padding
    pub unused: u32,
}

impl Header {
    pub fn ensure_integrity(&self) -> Result<(), Error> {
        if self.signature != [0x45, 0x46, 0x49, 0x20, 0x50, 0x41, 0x52, 0x54] {
            bail!("Bad signature {:x?}", self.signature)
        }
        if self.revision != 0x10000 {
            bail!("Bad revision {:x}", self.revision)
        }
        // TODO(https://fxbug.dev/339491886) more checks (crc, reasonable lbas, num_parts, etc)
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
        // TODO(https://fxbug.dev/339491886): checks
        Ok(())
    }
}

#[cfg(test)]
pub mod testing {
    use super::{Header, PartitionTableEntry};
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

        // TODO(https://fxbug.dev/339491886): Fill in secondary table too
        let header = Header {
            signature: [0x45, 0x46, 0x49, 0x20, 0x50, 0x41, 0x52, 0x54],
            revision: 0x10000,
            header_size: std::mem::size_of::<Header>() as u32,
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
            unused: 0,
        };

        let write_header = |offset: u64| {
            let header_raw = header.as_bytes();
            vmo.write(header_raw, offset).unwrap();
        };
        let write_partition_table = |mut offset: u64| {
            for partition in &partitions {
                let part = partition.as_entry();
                assert!(part.first_lba >= header.first_usable);
                assert!(part.last_lba <= header.last_usable);
                let part_raw = part.as_bytes();
                assert!(part_raw.len() <= part_size);
                vmo.write(part_raw, offset).unwrap();
                offset += part_size as u64;
            }
        };

        write_header(header.current_lba * bs);
        write_header(header.backup_lba * bs);
        write_partition_table((1 + header.current_lba) * bs);
        write_partition_table((header.backup_lba - partition_table_nblocks) * bs);
    }
}
