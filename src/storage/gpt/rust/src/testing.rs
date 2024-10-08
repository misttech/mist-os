// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use gpt::format::{self, Header};
use zerocopy::IntoBytes as _;

// Re-export `PartitionInfo` and its transitive dependencies.
pub use gpt::{Guid, PartitionInfo};

/// Formats `vmo` with the given partition table.  Assumes the VMO is big enough for all
/// partitions to be at a valid offset.
pub fn format_gpt(vmo: &zx::Vmo, block_size: u32, partitions: Vec<PartitionInfo>) {
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

    let partition_table = partitions.into_iter().map(|v| v.as_entry()).collect::<Vec<_>>();
    let partition_table_raw = format::serialize_partition_table(
        &mut header,
        block_size as usize,
        nblocks,
        &partition_table[..],
    )
    .unwrap();

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

    write_partition_table(&partition_table_raw[..], 2 * bs);
    write_partition_table(&partition_table_raw[..], (nblocks - 1 - partition_table_nblocks) * bs);
}
