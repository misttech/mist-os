// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod format;
mod metadata;

use crate::metadata::{round_up_to_alignment, SuperDeviceRange, SuperMetadata};
use anyhow::{anyhow, ensure, Error};
use async_trait::async_trait;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::sync::Arc;
use storage_device::buffer::{BufferRef, MutableBufferRef};
use storage_device::buffer_allocator::BufferFuture;
use storage_device::{Device, WriteOptions};

/// Struct to help interpret the deserialized "super" image.
pub struct SuperParser {
    super_metadata: SuperMetadata,
    used_regions: BTreeSet<SuperDeviceRange>,
    device: Arc<dyn Device>,
}

impl SuperParser {
    pub async fn new(device: Arc<dyn Device>) -> Result<Self, Error> {
        let super_metadata = SuperMetadata::load_from_device(device.as_ref()).await?;
        let super_metadata_size = super_metadata.geometry.get_total_metadata_size()?;

        let mut used_regions = BTreeSet::new();
        used_regions.insert(SuperDeviceRange(0..super_metadata_size));

        for metadata_slot in &super_metadata.metadata_slots {
            used_regions.append(&mut metadata_slot.get_all_used_extents_as_byte_range()?);
        }

        Ok(Self {
            super_metadata,
            used_regions: into_merged_regions(used_regions),
            device: device.clone(),
        })
    }

    /// Returns a vector of the used regions in-order, as a half-open Range(start..end). Note that
    /// the results would be more meaningful for extents with target type `TARGET_TYPE_LINEAR` as
    /// it implies that the extent is a dm-linear target which are made by concatenating linear
    /// regions (extents) of disk together. For `TARGET_TYPE_ZERO`, this would return [Range(0..0)].
    pub fn used_regions_in_bytes(&self) -> Vec<Range<u64>> {
        self.used_regions.clone().into_iter().map(|r| r.into()).collect()
    }

    /// Get a partition within super. Must specify the name of the sub-partition and which slot from
    /// super to read from.
    pub async fn get_sub_partition(
        &self,
        name: &str,
        slot_index: usize,
    ) -> Result<SuperPartitionDevice, Error> {
        let metadata_slot = &self.super_metadata.metadata_slots[slot_index];
        let extent_locations = metadata_slot.extent_locations_for_partition(name)?;
        assert_eq!(self.block_size(), self.device.block_size());
        Ok(SuperPartitionDevice::new(self.device.clone(), extent_locations).await?)
    }

    fn block_size(&self) -> u32 {
        self.super_metadata.block_size()
    }
}

fn into_merged_regions(mut regions: BTreeSet<SuperDeviceRange>) -> BTreeSet<SuperDeviceRange> {
    let mut merged_used_regions = BTreeSet::new();
    // BTreeSet will pop the regions in order (the ranges are sorted by the start of the range
    // first followed by the end).
    let mut current = regions.pop_first();
    if let Some(current_region) = &mut current {
        while let Some(next_region) = regions.pop_first() {
            if (*next_region).start > (*current_region).end {
                // This region is disjoint and it comes after `current_region`.
                merged_used_regions.insert(current_region.clone());
                *current_region = next_region;
            } else {
                // There is an overlap of regions - the start of this region is within the
                // current region. Update the end if needed.
                if (*next_region).end > (*current_region).end {
                    (*current_region).end = (*next_region).end;
                }
            }
        }
        // Insert the remaining region.
        merged_used_regions.insert(current_region.clone());
    }
    merged_used_regions
}

pub struct SuperPartitionDevice {
    device: Arc<dyn Device>,
    // Stores mapping of the (inclusive) end of the logical range to the physical range (which is
    // a half-open bounded range. So for `x` in the range (start..end), start <= x < end).
    extents_map: BTreeMap<u64, SuperDeviceRange>,
    partition_size_in_bytes: u64,
}

impl SuperPartitionDevice {
    pub async fn new(
        device: Arc<dyn Device>,
        extent_locations: Vec<SuperDeviceRange>,
    ) -> Result<Self, Error> {
        let mut extent_map = BTreeMap::new();
        let mut cursor = 0;
        for physical_range in &extent_locations {
            let size = physical_range.end - physical_range.start;
            cursor += size;
            // Subtract 1 to get the inclusive end of the logical range.
            extent_map.insert(cursor - 1, physical_range.clone());
        }
        Ok(Self {
            device: device.clone(),
            extents_map: extent_map,
            partition_size_in_bytes: round_up_to_alignment(cursor, device.block_size() as u64)?,
        })
    }
}

#[async_trait]
impl Device for SuperPartitionDevice {
    fn allocate_buffer(&self, size: usize) -> BufferFuture<'_> {
        self.device.allocate_buffer(size)
    }

    fn block_size(&self) -> u32 {
        self.device.block_size() as u32
    }

    fn block_count(&self) -> u64 {
        self.partition_size_in_bytes / self.block_size() as u64
    }

    async fn read(&self, offset: u64, mut buffer: MutableBufferRef<'_>) -> Result<(), Error> {
        let block_size = self.block_size() as u64;
        ensure!(offset % block_size == 0, "misaligned read at offset");
        ensure!(buffer.len() % block_size as usize == 0, "misaligned read for buffer length");
        if buffer.len() == 0 {
            return Ok(());
        }

        // Read may have to be split across multiple sub-reads across different extents.
        let mut logical_cursor = offset;
        let mut buffer_offset = 0;

        // Find the first extent to read from.
        let mut extents = self.extents_map.range(logical_cursor..);
        let (&logical_range_inclusive_end, mut physical_range) =
            extents.next().ok_or_else(|| anyhow!("Offset {} is out of bounds", offset))?;
        let mut extent_len = physical_range.end - physical_range.start;
        let mut logical_range =
            (logical_range_inclusive_end + 1 - extent_len)..(logical_range_inclusive_end + 1);

        while buffer_offset < buffer.len() {
            // Jump to the next extent if we're past the end of the current one.
            if logical_cursor >= logical_range.end {
                let (next_logical_range_inclusive_end, next_physical_range) = extents
                    .next()
                    .ok_or_else(|| anyhow!("Offset {} is out of bounds", logical_cursor))?;
                physical_range = next_physical_range;
                extent_len = physical_range.end - physical_range.start;
                logical_range = (next_logical_range_inclusive_end + 1 - extent_len)
                    ..(next_logical_range_inclusive_end + 1);
            }

            let physical_cursor = physical_range.start + (logical_cursor - logical_range.start);
            let subbuffer_len = std::cmp::min(
                (logical_range.end - logical_cursor) as usize,
                buffer.len() - buffer_offset,
            );
            ensure!(physical_cursor % block_size == 0, "misaligned read at offset");
            ensure!(subbuffer_len % block_size as usize == 0, "misaligned read for buffer length");
            self.device
                .read(
                    physical_cursor,
                    buffer.reborrow().subslice_mut(buffer_offset..buffer_offset + subbuffer_len),
                )
                .await?;
            buffer_offset += subbuffer_len;
            logical_cursor += subbuffer_len as u64;
        }
        Ok(())
    }

    async fn close(&self) -> Result<(), Error> {
        Ok(())
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn supports_trim(&self) -> bool {
        false
    }

    async fn write_with_opts(
        &self,
        _offset: u64,
        _buffer: BufferRef<'_>,
        _opts: WriteOptions,
    ) -> Result<(), Error> {
        Err(anyhow!("read-only partition"))
    }

    async fn flush(&self) -> Result<(), Error> {
        Err(anyhow!("read-only partition"))
    }

    async fn trim(&self, _range: Range<u64>) -> Result<(), Error> {
        Err(anyhow!("read-only partition"))
    }

    fn barrier(&self) {
        unreachable!()
    }
}

#[cfg(test)]
mod tests {
    use crate::{into_merged_regions, SuperDeviceRange, SuperParser};
    use rand::Rng as _;
    use std::collections::BTreeSet;
    use std::path::Path;
    use std::sync::Arc;
    use storage_device::fake_device::FakeDevice;
    use storage_device::Device;

    const BLOCK_SIZE: u32 = 4096;
    const IMAGE_PATH: &str = "/pkg/data/simple_super.img.zstd";

    fn open_image(path: &Path) -> Arc<FakeDevice> {
        let file = std::fs::File::open(path).expect("open file failed");
        let image = zstd::Decoder::new(file).expect("decompress image failed");
        Arc::new(
            FakeDevice::from_image(image, BLOCK_SIZE).expect("create fake block device failed"),
        )
    }

    #[fuchsia::test]
    async fn test_super_parser() {
        let device = open_image(std::path::Path::new(IMAGE_PATH));
        let super_parser = SuperParser::new(device.clone()).await.expect("SuperParser::new failed");
        let used_regions = super_parser.used_regions_in_bytes();
        // This is the expected used region for this test super image. This may need to be updated
        // if the super image changes.
        assert_eq!(used_regions, vec![(0..28672), (1048576..1056768), (2097152..2101248)]);
        device.close().await.expect("failed to close device");
    }

    #[fuchsia::test]
    async fn test_merging_regions() {
        let mut unmerged_regions = BTreeSet::new();
        // Case 1: two adjacent regions
        unmerged_regions.insert(SuperDeviceRange(0..1));
        unmerged_regions.insert(SuperDeviceRange(1..2));
        // Case 2: a fully overlapping region
        unmerged_regions.insert(SuperDeviceRange(0..2));
        // Case 3: a region contained within another
        unmerged_regions.insert(SuperDeviceRange(5..10));
        unmerged_regions.insert(SuperDeviceRange(7..8));
        // Case 4: partially overlapping region
        unmerged_regions.insert(SuperDeviceRange(15..20));
        unmerged_regions.insert(SuperDeviceRange(13..18));
        // Case 5: partially overlapping region (only the ends are different).
        unmerged_regions.insert(SuperDeviceRange(25..27));
        unmerged_regions.insert(SuperDeviceRange(25..30));

        let merged_regions: Vec<SuperDeviceRange> =
            into_merged_regions(unmerged_regions).into_iter().collect();
        assert_eq!(
            merged_regions,
            vec![
                SuperDeviceRange(0..2),
                SuperDeviceRange(5..10),
                SuperDeviceRange(13..20),
                SuperDeviceRange(25..30)
            ]
        );
    }

    #[fuchsia::test]
    async fn test_get_sub_partition() {
        let parent_device = open_image(std::path::Path::new(IMAGE_PATH));

        // The partition contains only zero in the simple test image, add some more interesting bits
        // to make sure we are reading the contents correctly.
        let super_parser =
            SuperParser::new(parent_device.clone()).await.expect("SuperParser::new failed");
        let used_regions = super_parser.used_regions_in_bytes();
        // We expect the used regions to contain [ region_of_metadata, logical_partitions... ]
        let start_of_first_partition = used_regions[1].start;
        let random_buffer: Vec<u8> =
            (0..8192).map(|_| rand::thread_rng().gen_range(0..100)).collect();
        let mut modified_buffer = parent_device.allocate_buffer(8192).await;
        modified_buffer.as_mut_slice().copy_from_slice(&random_buffer);
        parent_device
            .write(start_of_first_partition, modified_buffer.as_ref())
            .await
            .expect("failed to write to device");

        let system_partition = super_parser
            .get_sub_partition("system_a", 0)
            .await
            .expect("failed to load partition device");
        assert_eq!(system_partition.block_size(), 4096);
        assert_eq!(system_partition.block_count(), 2);
        // Verify the contents of this sub-partition
        let mut read_buffer = system_partition.allocate_buffer(8192).await;
        system_partition.read(0, read_buffer.as_mut()).await.expect("failed to read from device");
        assert_eq!(read_buffer.as_slice(), modified_buffer.as_slice());
        // Check that we can read from a non-0 offset
        let mut read_buffer = system_partition.allocate_buffer(4096).await;
        system_partition
            .read(4096, read_buffer.as_mut())
            .await
            .expect("failed to read from device");
        assert_eq!(read_buffer.as_slice(), &modified_buffer.as_slice()[4096..]);

        // Read the contents of the next partition. The simple test super image is set up such that
        // this should just be zeroes.
        let system_ext_partition = super_parser
            .get_sub_partition("system_ext_a", 0)
            .await
            .expect("failed to load partition device");

        assert_eq!(system_ext_partition.block_size(), 4096);
        assert_eq!(system_ext_partition.block_count(), 1);
        let mut read_buffer = system_ext_partition.allocate_buffer(4096).await;
        system_ext_partition
            .read(0, read_buffer.as_mut())
            .await
            .expect("failed to read from device");
        assert_eq!(read_buffer.as_slice(), [0; 4096]);
    }

    #[fuchsia::test]
    async fn test_misaligned_read_sub_partition_should_fail() {
        let parent_device = open_image(std::path::Path::new(IMAGE_PATH));

        let super_parser =
            SuperParser::new(parent_device.clone()).await.expect("SuperParser::new failed");

        let system_partition = super_parser
            .get_sub_partition("system_a", 0)
            .await
            .expect("failed to load partition device");

        // Test reading when buffer is not a multiple of block size
        let mut read_buffer = system_partition.allocate_buffer(3).await;
        system_partition
            .read(0, read_buffer.as_mut())
            .await
            .expect_err("misaligned read from device passes unexpectedly");

        // Test reading from misaligned buffer
        let mut read_buffer = system_partition.allocate_buffer(4096).await;
        system_partition
            .read(7, read_buffer.as_mut())
            .await
            .expect_err("misaligned read from device passes unexpectedly");
    }

    #[fuchsia::test]
    async fn test_out_of_bounds_read_sub_partition_should_fail() {
        let parent_device = open_image(std::path::Path::new(IMAGE_PATH));

        let super_parser =
            SuperParser::new(parent_device.clone()).await.expect("SuperParser::new failed");

        let system_partition = super_parser
            .get_sub_partition("system_a", 0)
            .await
            .expect("failed to load partition device");

        let block_size = system_partition.block_size() as usize;
        let block_count = system_partition.block_count() as usize;
        let out_of_bounds_len = (block_count + 1) * (block_size);

        // Test reading when buffer is out of bounds
        let mut read_buffer = system_partition.allocate_buffer(out_of_bounds_len).await;
        system_partition
            .read(0, read_buffer.as_mut())
            .await
            .expect_err("out of bounds read from device passes unexpectedly");

        // Test reading when reading from out of bounds
        let mut read_buffer = system_partition.allocate_buffer(4096).await;
        system_partition
            .read(out_of_bounds_len as u64, read_buffer.as_mut())
            .await
            .expect_err("out of bounds read from device passes unexpectedly");
    }
}
