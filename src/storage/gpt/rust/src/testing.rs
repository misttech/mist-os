// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crc::Hasher32 as _;

use gpt::format::{Header, PartitionTableEntry};
use zerocopy::IntoBytes as _;

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
            last_lba: self.start_block + self.num_blocks.saturating_sub(1),
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

    let mut header = Header::new(nblocks, block_size, partitions.len() as u32).unwrap();

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
