// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod format;

use crate::format::{
    BlockDeviceFlags, MetadataExtent, MetadataGeometry, MetadataHeader, PartitionGroupFlags,
    METADATA_GEOMETRY_RESERVED_SIZE, PARTITION_RESERVED_BYTES,
};
use anyhow::{anyhow, ensure, Error};
use format::{
    AdjustSlotSuffix, MetadataBlockDevice, MetadataPartition, MetadataPartitionGroup,
    MetadataTableDescriptor, ValidateTable,
};
use sha2::Digest;
use storage_device::fake_device::FakeDevice;
use storage_device::Device;
use zerocopy::{FromBytes, KnownLayout};

fn round_up_to_alignment(x: u64, alignment: u64) -> Result<u64, Error> {
    ensure!(alignment.count_ones() == 1, "alignment should be a power of 2");
    alignment
        .checked_mul(x.div_ceil(alignment))
        .ok_or(anyhow!("overflow occurred when rounding up to nearest alignment"))
}

// TODO(https://fxbug.dev/404952286): Add fuzzer to check for arithmetic overflows.

/// Struct to help interpret the deserialized "super" metadata.
#[derive(Debug)]
pub struct Metadata {
    geometry: MetadataGeometry,
    header: MetadataHeader,
    partitions: Vec<MetadataPartition>,
    extents: Vec<MetadataExtent>,
    partition_groups: Vec<MetadataPartitionGroup>,
    block_devices: Vec<MetadataBlockDevice>,
}

impl Metadata {
    /// Deserialize the "super" metadata. Caller should indicate which slot_number to read from. For
    /// example, for an A/B device, there will be two copies of metadata: "slot_a" is stored at slot
    /// 0 and "slot_b" is stored at slot 1.
    pub async fn load_from_device(device: &FakeDevice, slot_number: u32) -> Result<Self, Error> {
        let geometry = Self::load_metadata_geometry(&device).await?;

        ensure!(slot_number < geometry.metadata_slot_count, "Invalid metadata slot count");

        let mut metadata = match Self::load_metadata(
            &device,
            geometry,
            geometry.get_primary_metadata_offset(slot_number)?,
        )
        .await
        {
            Ok(metadata) => metadata,
            Err(_) => {
                Self::load_metadata(
                    &device,
                    geometry,
                    geometry.get_backup_metadata_offset(slot_number)?,
                )
                .await?
            }
        };

        // Need to update names for slot suffix before returning metadata.
        metadata.adjust_for_slot_suffix(slot_number)?;

        Ok(metadata)
    }

    // Load and validate geometry information from a block device that holds logical partitions.
    async fn load_metadata_geometry(device: &FakeDevice) -> Result<MetadataGeometry, Error> {
        // Read the primary geometry
        match Self::parse_metadata_geometry(&device, PARTITION_RESERVED_BYTES as u64).await {
            Ok(geometry) => Ok(geometry),
            Err(_) => {
                // Try the backup geometry
                Self::parse_metadata_geometry(
                    &device,
                    PARTITION_RESERVED_BYTES as u64 + METADATA_GEOMETRY_RESERVED_SIZE as u64,
                )
                .await
            }
        }
    }

    async fn load_metadata(
        device: &FakeDevice,
        geometry: MetadataGeometry,
        offset: u64,
    ) -> Result<Self, Error> {
        let header = Self::parse_metadata_header(&device, offset).await?;

        // Now that we have more information on the tables, validate table-specific fields.
        ensure!(header.tables_size <= geometry.metadata_max_size, "Invalid tables size.");

        // Read tables bytes from device
        ensure!(offset % device.block_size() as u64 == 0, "Reads must be block aligned.");
        let header_and_tables_size = header
            .header_size
            .checked_add(header.tables_size)
            .ok_or_else(|| anyhow!("calculate header and tables size: arithmetic overflow"))?;
        let buffer_len =
            round_up_to_alignment(header_and_tables_size as u64, device.block_size() as u64)?;
        let mut buffer = device.allocate_buffer(buffer_len.try_into()?).await;
        device.read(offset, buffer.as_mut()).await?;
        let tables_bytes = &buffer.as_slice()
            [header.header_size.try_into()?..header_and_tables_size.try_into()?];

        // Read tables to verify `table_checksum` in metadata_header.
        let computed_tables_checksum: [u8; 32] = sha2::Sha256::digest(tables_bytes).into();
        ensure!(
            computed_tables_checksum == header.tables_checksum,
            "Invalid metadata tables checksum"
        );

        // Parse partition table entries.
        let read_cursor = 0;
        let partitions = Self::parse_table::<MetadataPartition>(
            tables_bytes,
            read_cursor,
            &header,
            &header.partitions,
        )
        .await?;

        // Parse extent table entries.
        let read_cursor =
            read_cursor.checked_add(header.partitions.get_table_size()?).ok_or_else(|| {
                anyhow!("calculate cursor to read extent entries: arithmetic overflow")
            })?;
        let extents = Self::parse_table::<MetadataExtent>(
            tables_bytes,
            read_cursor,
            &header,
            &header.extents,
        )
        .await?;

        // Parse partition group table entries.
        let read_cursor =
            read_cursor.checked_add(header.extents.get_table_size()?).ok_or_else(|| {
                anyhow!("calculate cursor to read group entries: arithmetic overflow")
            })?;
        let partition_groups = Self::parse_table::<MetadataPartitionGroup>(
            tables_bytes,
            read_cursor,
            &header,
            &header.groups,
        )
        .await?;

        // Parse block device table entries.
        let read_cursor =
            read_cursor.checked_add(header.groups.get_table_size()?).ok_or_else(|| {
                anyhow!("calculate cursor to read block devices entries: arithmetic overflow")
            })?;
        let block_devices = Self::parse_table::<MetadataBlockDevice>(
            tables_bytes,
            read_cursor,
            &header,
            &header.block_devices,
        )
        .await?;

        // Expect there to be at least be one block device: "super".
        ensure!(block_devices.len() > 0, "Metadata did not specify a super device.");
        let super_device = block_devices[0];
        let logical_partition_offset = super_device.get_first_logical_sector_in_bytes()?;
        let metadata_region = geometry.get_total_metadata_size()?;
        ensure!(
            metadata_region <= logical_partition_offset,
            "Logical partition metadata overlaps with logical partition contents."
        );

        Ok(Self { geometry, header, partitions, extents, partition_groups, block_devices })
    }

    // Read and validates the metadata geometry. The offset provided will be rounded up to the
    // nearest block alignment.
    async fn parse_metadata_geometry(
        device: &FakeDevice,
        offset: u64,
    ) -> Result<MetadataGeometry, Error> {
        let buffer_len = round_up_to_alignment(
            METADATA_GEOMETRY_RESERVED_SIZE as u64,
            device.block_size() as u64,
        )?;
        let mut buffer = device.allocate_buffer(buffer_len.try_into()?).await;
        let aligned_offset = round_up_to_alignment(offset, device.block_size() as u64)?;
        device.read(aligned_offset, buffer.as_mut()).await?;
        let full_buffer = buffer.as_slice();
        let (metadata_geometry, _remainder) = MetadataGeometry::read_from_prefix(full_buffer)
            .map_err(|e| anyhow!("Failed to read metadata geometry: {e}"))?;
        metadata_geometry.validate()?;
        Ok(metadata_geometry)
    }

    async fn parse_metadata_header(
        device: &FakeDevice,
        offset: u64,
    ) -> Result<MetadataHeader, Error> {
        // Reads must be block aligned.
        ensure!(offset % device.block_size() as u64 == 0, "Reads must be block aligned.");
        let buffer_len = round_up_to_alignment(
            std::mem::size_of::<MetadataHeader>() as u64,
            device.block_size() as u64,
        )?;
        let mut buffer = device.allocate_buffer(buffer_len.try_into()?).await;
        device.read(offset, buffer.as_mut()).await?;

        let full_buffer = buffer.as_slice();
        let (mut metadata_header, _remainder) = MetadataHeader::read_from_prefix(full_buffer)
            .map_err(|e| anyhow!("Failed to read metadata header: {e}"))?;
        // Validation will also check if the header is an older version, and if so, will zero the
        // fields in `MetadataHeader` that did not exist in the older version.
        metadata_header.validate()?;
        Ok(metadata_header)
    }

    async fn parse_table<T: ValidateTable + FromBytes + KnownLayout>(
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
                .ok_or_else(|| anyhow!("calculate bytes read so far: arithmetic overflow"))?;
            let offset = tables_offset
                .checked_add(read_so_far)
                .ok_or_else(|| anyhow!("calculate table bytes offset: arithmetic overflow"))?;
            let (entry, _remainder) = T::read_from_prefix(&tables_bytes[offset.try_into()?..])
                .map_err(|e| anyhow!("Failed to read table: {e}"))?;
            entry.validate(metadata_header)?;
            entries.push(entry);
        }
        Ok(entries)
    }

    fn adjust_for_slot_suffix(&mut self, slot_number: u32) -> Result<(), Error> {
        for partition in &mut self.partitions {
            partition.adjust_for_slot_suffix(slot_number)?;
        }

        for block_device in &mut self.block_devices {
            block_device.adjust_for_slot_suffix(slot_number)?;
        }

        for partition_group in &mut self.partition_groups {
            partition_group.adjust_for_slot_suffix(slot_number)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::format::{PartitionAttributes, RESERVED_AND_GEOMETRIES_SIZE, SECTOR_SIZE};

    use super::*;
    use std::path::Path;
    use storage_device::fake_device::FakeDevice;

    const BLOCK_SIZE: u32 = 4096;
    const IMAGE_PATH: &str = "/pkg/data/simple_super.img.zstd";
    const IMAGE_METADATA_MAX_SIZE: u32 = 4096;

    fn open_image(path: &Path) -> FakeDevice {
        // If image changes at the file path, need to update the `verify_*` functions below that
        // verifies the parser and the test const `IMAGE_METADATA_MAX_SIZE`.
        let file = std::fs::File::open(path).expect("open file failed");
        let image = zstd::Decoder::new(file).expect("decompress image failed");
        FakeDevice::from_image(image, BLOCK_SIZE).expect("create fake block device failed")
    }

    // Verify metadata geometry against the image at `IMAGE_PATH`.
    fn verify_geometry(metadata: &Metadata) -> Result<(), Error> {
        let geometry = metadata.geometry;
        let max_size = geometry.metadata_max_size;
        let metadata_slot_count = geometry.metadata_slot_count;
        assert_eq!(max_size, IMAGE_METADATA_MAX_SIZE);
        assert_eq!(metadata_slot_count, 2);
        Ok(())
    }

    // Verify metadata header against the image at `IMAGE_PATH`.
    fn verify_header(metadata: &Metadata) -> Result<(), Error> {
        let header = metadata.header;
        let num_partition_entries = header.partitions.num_entries;
        let num_extent_entries = header.extents.num_entries;
        assert_eq!(num_partition_entries, 2);
        assert_eq!(num_extent_entries, 2);
        Ok(())
    }

    // Verify metadata partitions table against the image at `IMAGE_PATH`.
    fn verify_partitions_table(metadata: &Metadata, slot_number: u32) -> Result<(), Error> {
        let partitions = &metadata.partitions;
        // The first entry in the partitions table is "system".
        let partition_name = String::from_utf8(partitions[0].name.to_vec())
            .expect("failed to convert partition entry name to string");
        let expected_name = match slot_number {
            0 => "system_a".to_string(),
            1 => "system_b".to_string(),
            _ => return Err(anyhow!("unexpected slot number: should be 0 or 1")),
        };
        assert_eq!(partition_name.trim_end_matches(|c| c == '\0'), expected_name);
        let partition_attributes = partitions[0].attributes;
        assert_eq!(partition_attributes, PartitionAttributes::READONLY);
        let system_partition_extent_index = partitions[0].first_extent_index;
        let system_partition_num_extents = partitions[0].num_extents;
        assert_eq!(system_partition_extent_index, 0);
        assert_eq!(system_partition_num_extents, 1);
        // The slot suffix flag should be removed after parsing Metadata.
        let attributes = partitions[0].attributes;
        assert!(!attributes.contains(PartitionAttributes::SLOT_SUFFIXED));

        // The next entry in the partitions table is "system_ext".
        let partition_name = String::from_utf8(partitions[1].name.to_vec())
            .expect("failed to convert partition entry name to string");
        let expected_name = match slot_number {
            0 => "system_ext_a".to_string(),
            1 => "system_ext_b".to_string(),
            _ => return Err(anyhow!("unexpected slot number: should be 0 or 1")),
        };
        assert_eq!(partition_name.trim_end_matches(|c| c == '\0'), expected_name);
        let partition_attributes = partitions[1].attributes;
        assert_eq!(partition_attributes, PartitionAttributes::READONLY);
        let system_partition_extent_index = partitions[1].first_extent_index;
        let system_partition_num_extents = partitions[1].num_extents;
        assert_eq!(system_partition_extent_index, 1);
        assert_eq!(system_partition_num_extents, 1);
        let attributes = partitions[1].attributes;
        assert!(!attributes.contains(PartitionAttributes::SLOT_SUFFIXED));
        Ok(())
    }

    // Verify metadata extents table against the image at `IMAGE_PATH`.
    fn verify_extents_table(metadata: &Metadata) -> Result<(), Error> {
        // The simple super image has a "system" partition of size 8192 bytes. This extent entry
        // refers to the extent used by that partition.
        let num_sectors = metadata.extents[0].num_sectors;
        assert_eq!(num_sectors * SECTOR_SIZE as u64, 8192);

        // The simple super image has a "system_ext" partition of size 4096 bytes. This extent entry
        // refers to the extent used by that partition.
        let num_sectors = metadata.extents[1].num_sectors;
        assert_eq!(num_sectors * SECTOR_SIZE as u64, 4096);
        Ok(())
    }

    // Verify metadata partition groups table against the image at `IMAGE_PATH`.
    fn verify_partition_groups_table(metadata: &Metadata, slot_numer: u32) -> Result<(), Error> {
        // Expect to see two groups, "default" of unlimited maximum size and "example" of size 0.
        assert_eq!(metadata.partition_groups.len(), 2);

        // The default group does not have a slot suffix applied.
        let default_group = metadata.partition_groups[0];
        let name = String::from_utf8(default_group.name.to_vec())
            .expect("failed to convert partition group name to string");
        assert_eq!(name.trim_end_matches(|c| c == '\0'), "default".to_string());
        let maximum_size = default_group.maximum_size;
        assert_eq!(maximum_size, 0);

        let group = metadata.partition_groups[1];
        let name = String::from_utf8(group.name.to_vec())
            .expect("failed to convert partition group name to string");
        let expected_name = match slot_numer {
            0 => "example_a".to_string(),
            1 => "example_b".to_string(),
            _ => return Err(anyhow!("unexpected slot number: should be 0 or 1")),
        };
        assert_eq!(name.trim_end_matches(|c| c == '\0'), expected_name);
        let maximum_size = group.maximum_size;
        assert_eq!(maximum_size, 0);

        // The slot suffix flag should be removed after parsing Metadata.
        let group_flag = group.flags;
        assert!(!group_flag.contains(PartitionGroupFlags::SLOT_SUFFIXED));
        Ok(())
    }

    // Verify metadata block devices table against the image at `IMAGE_PATH`.
    fn verify_block_devices_table(metadata: &Metadata, slot_number: u32) -> Result<(), Error> {
        let block_devices = &metadata.block_devices;
        let partition_name = String::from_utf8(block_devices[0].partition_name.to_vec())
            .expect("failed to convert partition entry name to string");
        let expected_name = match slot_number {
            0 => "super_a".to_string(),
            1 => "super_b".to_string(),
            _ => return Err(anyhow!("unexpected slot number: should be 0 or 1")),
        };
        assert_eq!(partition_name.trim_end_matches(|c| c == '\0'), expected_name);
        let device_size = block_devices[0].size;
        assert_eq!(device_size, 4194304);
        let flag = block_devices[0].flags;
        assert!(!flag.contains(BlockDeviceFlags::SLOT_SUFFIXED));
        Ok(())
    }

    fn verify_super_metadata(metadata: &Metadata, slot_number: u32) -> Result<(), Error> {
        verify_geometry(metadata).expect("incorrect geometry");
        verify_header(metadata).expect("incorrect header");
        verify_partitions_table(metadata, slot_number).expect("incorrect partitions table");
        verify_extents_table(metadata).expect("incorrect extents table");
        verify_partition_groups_table(metadata, slot_number)
            .expect("incorrect partition groups table");
        verify_block_devices_table(metadata, slot_number).expect("incorrect block devices table");
        Ok(())
    }

    #[fuchsia::test]
    async fn test_parsing_metadata() {
        let device = open_image(std::path::Path::new(IMAGE_PATH));

        let super_partition =
            Metadata::load_from_device(&device, 0).await.expect("failed to load super metatata.");

        verify_super_metadata(&super_partition, 0).expect("incorrect metadata");
        device.close().await.expect("failed to close device");
    }

    #[fuchsia::test]
    async fn test_parsing_metadata_with_invalid_primary_geometry() {
        let device = open_image(std::path::Path::new(IMAGE_PATH));

        // Corrupt the primary geometry bytes.
        {
            let offset =
                round_up_to_alignment(PARTITION_RESERVED_BYTES as u64, device.block_size() as u64)
                    .expect("failed to round to nearest block");
            let buf_len = round_up_to_alignment(
                METADATA_GEOMETRY_RESERVED_SIZE as u64,
                device.block_size() as u64,
            )
            .expect("failed to round to nearest block");
            let mut buf = device.allocate_buffer(buf_len as usize).await;
            buf.as_mut_slice().fill(0xaa as u8);
            device.write(offset, buf.as_ref()).await.expect("failed to write to device");
        }

        let super_partition =
            Metadata::load_from_device(&device, 0).await.expect("failed to load super metatata.");
        verify_super_metadata(&super_partition, 0).expect("incorrect metadata");
        device.close().await.expect("failed to close device");
    }

    #[fuchsia::test]
    async fn test_parsing_metadata_with_invalid_primary_and_backup_geometry() {
        let device = open_image(std::path::Path::new(IMAGE_PATH));

        // Corrupt the primary and backup geometry bytes.
        {
            let offset =
                round_up_to_alignment(PARTITION_RESERVED_BYTES as u64, device.block_size() as u64)
                    .expect("failed to round to nearest block");
            let buf_len = round_up_to_alignment(
                2 * METADATA_GEOMETRY_RESERVED_SIZE as u64,
                device.block_size() as u64,
            )
            .expect("failed to round to nearest block");
            let mut buf = device.allocate_buffer(buf_len as usize).await;
            buf.as_mut_slice().fill(0xaa as u8);
            device.write(offset, buf.as_ref()).await.expect("failed to write to device");
        }

        Metadata::load_from_device(&device, 0)
            .await
            .expect_err("passed loading super metatata unexpectedly");
        device.close().await.expect("failed to close device");
    }

    #[fuchsia::test]
    async fn test_load_from_invalid_slot_number() {
        let device = open_image(std::path::Path::new(IMAGE_PATH));

        // This image has 2 metadata slots
        Metadata::load_from_device(&device, 5)
            .await
            .expect_err("passed loading super metatata unexpectedly");
        device.close().await.expect("failed to close device");
    }

    #[fuchsia::test]
    async fn test_parsing_metadata_with_invalid_primary_metadata() {
        let device = open_image(std::path::Path::new(IMAGE_PATH));

        // Corrupt only the primary metadata bytes.
        {
            let offset = round_up_to_alignment(
                2 * IMAGE_METADATA_MAX_SIZE as u64,
                device.block_size() as u64,
            )
            .expect("failed to round to nearest block");
            let buf_len = round_up_to_alignment(100 as u64, device.block_size() as u64)
                .expect("failed to round to nearest block");
            let mut buf = device.allocate_buffer(buf_len as usize).await;
            buf.as_mut_slice().fill(0xaa as u8);
            device.write(offset, buf.as_ref()).await.expect("failed to write to device");
        }

        let metadata_slot0 =
            Metadata::load_from_device(&device, 0).await.expect("failed to load super metatata.");
        verify_super_metadata(&metadata_slot0, 0).expect("incorrect metadata");

        let metadata_slot1 =
            Metadata::load_from_device(&device, 1).await.expect("failed to load super metatata.");
        verify_super_metadata(&metadata_slot1, 1).expect("incorrect metadata");
    }

    #[fuchsia::test]
    async fn test_parsing_metadata_with_invalid_primary_and_backup_metadata() {
        let device = open_image(std::path::Path::new(IMAGE_PATH));

        // Corrupt both the primary and backup metadata bytes.
        {
            let offset = round_up_to_alignment(
                RESERVED_AND_GEOMETRIES_SIZE as u64,
                device.block_size() as u64,
            )
            .expect("failed to round to nearest block");
            let buf_len = round_up_to_alignment(
                4 * IMAGE_METADATA_MAX_SIZE as u64,
                device.block_size() as u64,
            )
            .expect("failed to round to nearest block");
            let mut buf = device.allocate_buffer(buf_len as usize).await;
            buf.as_mut_slice().fill(0xaa as u8);
            device.write(offset, buf.as_ref()).await.expect("failed to write to device");
        }

        // Test loading from both slots
        Metadata::load_from_device(&device, 0)
            .await
            .expect_err("passed loading super metatata unexpectedly");
        Metadata::load_from_device(&device, 1)
            .await
            .expect_err("passed loading super metatata unexpectedly");
        device.close().await.expect("failed to close device");
    }
}
