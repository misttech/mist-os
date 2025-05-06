// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod format;

use crate::format::{
    MetadataGeometry, MetadataHeader, METADATA_GEOMETRY_RESERVED_SIZE, PARTITION_RESERVED_BYTES,
};
use anyhow::{anyhow, ensure, Error};
use sha2::Digest;
use storage_device::fake_device::FakeDevice;
use storage_device::Device;
use zerocopy::FromBytes;

fn round_up_to_alignment(x: u32, alignment: u32) -> Result<u32, Error> {
    ensure!(alignment.count_ones() == 1, "alignment should be a power of 2");
    alignment
        .checked_mul(x.div_ceil(alignment))
        .ok_or(anyhow!("overflow occurred when rounding up to nearest alignment"))
}

pub struct SuperParser {
    metadata_geometry: MetadataGeometry,
    metadata_header: MetadataHeader,
    // TODO(https://fxbug.dev/404952286): parse partitions, extents, groups, and block devices.
}

impl SuperParser {
    pub async fn open_device(device: FakeDevice) -> Result<Self, Error> {
        let metadata_geometry = Self::parse_metadata_geometry(&device).await.unwrap();
        let metadata_header = Self::parse_metadata_header(&device).await.unwrap();

        // Now that we have more information on the tables, validate table-specific fields.
        ensure!(
            metadata_header.tables_size <= metadata_geometry.metadata_max_size,
            "Invalid tables size."
        );
        // Read tables to verify `table_checksum` in metadata_header.
        let computed_tables_checksum =
            Self::compute_tables_checksum(&device, &metadata_header).await.unwrap();
        ensure!(
            computed_tables_checksum == metadata_header.tables_checksum,
            "Invalid metadata tables checksum"
        );

        let this = Self { metadata_geometry, metadata_header };
        Ok(this)
    }

    async fn parse_metadata_geometry(device: &FakeDevice) -> Result<MetadataGeometry, Error> {
        let buffer_len =
            round_up_to_alignment(METADATA_GEOMETRY_RESERVED_SIZE, device.block_size())?;
        let mut buffer = device.allocate_buffer(buffer_len as usize).await;
        ensure!(
            PARTITION_RESERVED_BYTES % device.block_size() == 0,
            "Read offsets must be block aligned."
        );
        device.read(PARTITION_RESERVED_BYTES as u64, buffer.as_mut()).await?;
        let full_buffer = buffer.as_slice();
        let (metadata_geometry, _remainder) =
            MetadataGeometry::read_from_prefix(full_buffer).unwrap();
        metadata_geometry.validate()?;
        Ok(metadata_geometry)
    }

    async fn parse_metadata_header(device: &FakeDevice) -> Result<MetadataHeader, Error> {
        // Reads must be block aligned.
        let metadata_header_offset = PARTITION_RESERVED_BYTES + 2 * METADATA_GEOMETRY_RESERVED_SIZE;
        ensure!(metadata_header_offset % device.block_size() == 0, "Reads must be block aligned.");
        let buffer_len = round_up_to_alignment(
            std::mem::size_of::<MetadataHeader>() as u32,
            device.block_size(),
        )?;
        let mut buffer = device.allocate_buffer(buffer_len as usize).await;
        device.read(metadata_header_offset as u64, buffer.as_mut()).await?;

        let full_buffer = buffer.as_slice();
        let (mut metadata_header, _remainder) =
            MetadataHeader::read_from_prefix(full_buffer).unwrap();
        // Validation will also check if the header is an older version, and if so, will zero the
        // fields in `MetadataHeader` that did not exist in the older version.
        metadata_header.validate()?;
        Ok(metadata_header)
    }

    async fn compute_tables_checksum(
        device: &FakeDevice,
        metadata_header: &MetadataHeader,
    ) -> Result<[u8; 32], Error> {
        // Reads should be block aligned. The start of the tables may not be block aligned, however,
        // the start of the metadata header should be. Note that a backup metadata geometry is
        // stored right after the initial metadata geometry.
        let metadata_header_offset = PARTITION_RESERVED_BYTES + 2 * METADATA_GEOMETRY_RESERVED_SIZE;
        ensure!(metadata_header_offset % device.block_size() == 0, "Reads must be block aligned.");
        let header_and_tables_size = metadata_header.header_size + metadata_header.tables_size;
        let buffer_len = round_up_to_alignment(header_and_tables_size, device.block_size())?;
        let mut buffer = device.allocate_buffer(buffer_len as usize).await;
        device.read(metadata_header_offset as u64, buffer.as_mut()).await?;

        let tables_bytes = &buffer.as_slice()
            [metadata_header.header_size as usize..header_and_tables_size as usize];
        Ok(sha2::Sha256::digest(tables_bytes).into())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use storage_device::fake_device::FakeDevice;

    fn open_image(path: &Path) -> FakeDevice {
        let image = zstd::Decoder::new(std::fs::File::open(path).expect("open file failed"))
            .expect("decompress image failed");
        FakeDevice::from_image(image, 4096 as u32).expect("create fake block device failed")
    }

    #[fuchsia::test]
    async fn test_parsing_metadata() {
        let golden = std::path::Path::new("/pkg/data/simple_super.img.zstd");
        let device = open_image(golden);

        let super_partition = SuperParser::open_device(device).await.expect("open device failed");

        let max_size = super_partition.metadata_geometry.metadata_max_size;
        let metadata_slot_count = super_partition.metadata_geometry.metadata_slot_count;
        assert_eq!(max_size, 65536);
        assert_eq!(metadata_slot_count, 1);

        let num_partition_entries = super_partition.metadata_header.partitions.num_entries;
        assert_eq!(num_partition_entries, 2);
    }
}
