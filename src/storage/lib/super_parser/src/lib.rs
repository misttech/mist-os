// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod format;

use crate::format::{
    MetadataExtent, MetadataGeometry, MetadataHeader, METADATA_GEOMETRY_RESERVED_SIZE,
    PARTITION_RESERVED_BYTES,
};
use anyhow::{anyhow, ensure, Error};
use format::{
    MetadataBlockDevice, MetadataPartition, MetadataPartitionGroup, MetadataTableDescriptor,
    ValidateTable,
};
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

// TODO(https://fxbug.dev/404952286): Add checks for potential arithmetic overflows from data read
// from the device and test for this.
// TODO(https://fxbug.dev/404952286): Add fuzzer to check for arithmetic overflows.

pub struct SuperParser {
    metadata_geometry: MetadataGeometry,
    metadata_header: MetadataHeader,
    partitions: Vec<MetadataPartition>,
    extents: Vec<MetadataExtent>,
    partition_groups: Vec<MetadataPartitionGroup>,
    block_devices: Vec<MetadataBlockDevice>,
}

impl SuperParser {
    pub async fn open_device(device: FakeDevice) -> Result<Self, Error> {
        let metadata_geometry = Self::parse_metadata_geometry(&device).await.unwrap();
        // TODO(https://fxbug.dev/404952286): read backup geometry if needed.

        let metadata_header = Self::parse_metadata_header(&device).await.unwrap();

        // Now that we have more information on the tables, validate table-specific fields.
        ensure!(
            metadata_header.tables_size <= metadata_geometry.metadata_max_size,
            "Invalid tables size."
        );

        // TODO(https://fxbug.dev/404952286): read backup metadata if we fail any of the validation
        // the first time.

        // Get tables bytes
        let metadata_header_offset = PARTITION_RESERVED_BYTES + 2 * METADATA_GEOMETRY_RESERVED_SIZE;
        ensure!(metadata_header_offset % device.block_size() == 0, "Reads must be block aligned.");
        let header_and_tables_size = metadata_header.header_size + metadata_header.tables_size;
        let buffer_len = round_up_to_alignment(header_and_tables_size, device.block_size())?;
        let mut buffer = device.allocate_buffer(buffer_len as usize).await;
        device.read(metadata_header_offset as u64, buffer.as_mut()).await?;
        let tables_bytes = &buffer.as_slice()
            [metadata_header.header_size as usize..header_and_tables_size as usize];

        // Read tables to verify `table_checksum` in metadata_header.
        let computed_tables_checksum: [u8; 32] = sha2::Sha256::digest(tables_bytes).into();
        ensure!(
            computed_tables_checksum == metadata_header.tables_checksum,
            "Invalid metadata tables checksum"
        );

        // Parse partition table entries.
        let tables_offset = 0;
        let partitions = Self::parse_table::<MetadataPartition>(
            tables_bytes,
            tables_offset,
            &metadata_header,
            &metadata_header.partitions,
        )
        .await
        .unwrap();

        // Parse extent table entries. Note that `num_entries * entry_size` has already been checked
        // that it does not overflow when parsing MetadataHeader above.
        let tables_offset = tables_offset
            .checked_add(
                metadata_header.partitions.num_entries * metadata_header.partitions.entry_size,
            )
            .ok_or_else(|| anyhow!("Adding offset + num_entries * entry_size overflowed."))?;
        let extents = Self::parse_table::<MetadataExtent>(
            tables_bytes,
            tables_offset,
            &metadata_header,
            &metadata_header.extents,
        )
        .await
        .unwrap();

        // Parse partition group table entries.
        let tables_offset = tables_offset
            .checked_add(metadata_header.extents.num_entries * metadata_header.extents.entry_size)
            .ok_or_else(|| anyhow!("Adding offset + num_entries * entry_size overflowed."))?;
        let partition_groups = Self::parse_table::<MetadataPartitionGroup>(
            tables_bytes,
            tables_offset,
            &metadata_header,
            &metadata_header.groups,
        )
        .await
        .unwrap();

        // Parse block device table entries.
        let tables_offset = tables_offset
            .checked_add(metadata_header.groups.num_entries * metadata_header.groups.entry_size)
            .ok_or_else(|| anyhow!("Adding offset + num_entries * entry_size overflowed."))?;
        let block_devices = Self::parse_table::<MetadataBlockDevice>(
            tables_bytes,
            tables_offset,
            &metadata_header,
            &metadata_header.block_devices,
        )
        .await
        .unwrap();

        // Expect there to be at least be one block device: "super".
        ensure!(block_devices.len() > 0, "Metadata did not specify a super device.");
        let super_device = block_devices[0];
        // The block device would have passed validation before. We don't expect this to return an
        // error.
        let logical_partition_offset = super_device.get_first_logical_sector_in_bytes()?;
        // `metadata_geometry` passed validation before. We don't expect this to return an error.
        let metadata_region = metadata_geometry.get_total_metadata_size()?;
        ensure!(
            metadata_region <= logical_partition_offset,
            "Logical partition metadata overlaps with logical partition contents."
        );

        Ok(Self {
            metadata_geometry,
            metadata_header,
            partitions,
            extents,
            partition_groups,
            block_devices,
        })
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

    async fn parse_table<T: ValidateTable + FromBytes>(
        tables_bytes: &[u8],
        tables_offset: u32,
        metadata_header: &MetadataHeader,
        table_descriptor: &MetadataTableDescriptor,
    ) -> Result<Vec<T>, Error> {
        let mut entries = Vec::new();
        let entry_size = table_descriptor.entry_size;
        let num_entries = table_descriptor.num_entries;
        for index in 0..num_entries {
            let read_so_far = index
                .checked_mul(entry_size)
                .ok_or_else(|| anyhow!("arithmetic overflow occurred"))?;
            let offset = tables_offset
                .checked_add(read_so_far)
                .ok_or_else(|| anyhow!("arithmetic overflow occurred"))?;
            let (entry, _remainder) =
                T::read_from_prefix(&tables_bytes[offset as usize..]).unwrap();
            entry.validate(metadata_header)?;
            entries.push(entry);
        }
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use crate::format::{PartitionAttributes, SECTOR_SIZE};

    use super::*;
    use std::path::Path;
    use storage_device::fake_device::FakeDevice;

    const BLOCK_SIZE: u32 = 4096;

    fn open_image(path: &Path) -> FakeDevice {
        let image = zstd::Decoder::new(std::fs::File::open(path).expect("open file failed"))
            .expect("decompress image failed");
        FakeDevice::from_image(image, BLOCK_SIZE).expect("create fake block device failed")
    }

    #[fuchsia::test]
    async fn test_parsing_metadata() {
        let golden = std::path::Path::new("/pkg/data/simple_super.img.zstd");
        let device = open_image(golden);

        let super_partition = SuperParser::open_device(device).await.expect("open device failed");

        // Check geometry.
        let max_size = super_partition.metadata_geometry.metadata_max_size;
        let metadata_slot_count = super_partition.metadata_geometry.metadata_slot_count;
        assert_eq!(max_size, 65536);
        assert_eq!(metadata_slot_count, 1);

        // Check header.
        let num_partition_entries = super_partition.metadata_header.partitions.num_entries;
        let num_extent_entries = super_partition.metadata_header.extents.num_entries;
        assert_eq!(num_partition_entries, 2);
        assert_eq!(num_extent_entries, 1);

        // Check partitions.
        let partitions = super_partition.partitions;
        let expected_name = "system".to_string();
        let partition_name = String::from_utf8(partitions[0].name.to_vec())
            .expect("failed to convert partition entry name to string");
        assert_eq!(partition_name[..expected_name.len()], expected_name);
        let partition_attributes = partitions[0].attributes;
        assert_eq!(partition_attributes, PartitionAttributes::READONLY);

        let expected_name = "system_ext".to_string();
        let partition_name = String::from_utf8(partitions[1].name.to_vec())
            .expect("failed to convert partition entry name to string");
        assert_eq!(partition_name[..expected_name.len()], expected_name);
        let partition_attributes = partitions[1].attributes;
        assert_eq!(partition_attributes, PartitionAttributes::READONLY);

        // "System" partition owns one extent starting at the 0-th extent index.
        let system_partition_extent_index = partitions[0].first_extent_index;
        let system_partition_num_extents = partitions[0].num_extents;
        assert_eq!(system_partition_extent_index, 0);
        assert_eq!(system_partition_num_extents, 1);

        // Check extents - the simple super image has a "system" partition of size 4096 bytes.
        let num_sectors = super_partition.extents[0].num_sectors;
        assert_eq!(num_sectors * SECTOR_SIZE as u64, 4096);

        // Expect to see one group "default" of unlimited maximum size.
        assert_eq!(super_partition.partition_groups.len(), 1);
        let group = super_partition.partition_groups[0];
        let expected_group_name = "default".to_string();
        let group_name = String::from_utf8(group.name.to_vec())
            .expect("failed to convert partition group name to string");
        assert_eq!(group_name[..expected_group_name.len()], expected_group_name);
        let maximum_size = group.maximum_size;
        assert_eq!(maximum_size, 0);

        let expected_partition_name = "super".to_string();
        let block_device_partition_name =
            String::from_utf8(super_partition.block_devices[0].partition_name.to_vec())
                .expect("failed to convert partition entry name to string");
        assert_eq!(
            block_device_partition_name[..expected_partition_name.len()],
            expected_partition_name
        );
        let device_size = super_partition.block_devices[0].size;
        assert_eq!(device_size, 1073741824);
    }
}
