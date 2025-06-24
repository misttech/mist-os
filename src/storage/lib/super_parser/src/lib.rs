// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod format;
mod metadata;

use crate::metadata::{SuperDeviceRange, SuperMetadata};
use std::collections::BTreeSet;
use std::ops::Range;
use std::sync::Arc;
use storage_device::Device;

use anyhow::Error;

/// Struct to help interpret the deserialized "super" image.
#[derive(Debug)]
pub struct SuperParser {
    used_regions: BTreeSet<SuperDeviceRange>,
}

impl SuperParser {
    pub async fn new(device: Arc<dyn Device>) -> Result<Self, Error> {
        let super_metadata = SuperMetadata::load_from_device(device.as_ref()).await?;
        let super_metadata_size = super_metadata.geometry.get_total_metadata_size()?;

        let mut used_regions = BTreeSet::new();
        used_regions.insert(SuperDeviceRange(0..super_metadata_size));

        for metadata_slot in &super_metadata.metadata_slots {
            used_regions.append(&mut metadata_slot.get_used_extents_as_byte_range()?);
        }

        Ok(Self { used_regions: into_merged_regions(used_regions) })
    }

    /// Returns a vector of the used regions in-order, as a half-open Range(start..end). Note that
    /// the results would be more meaningful for extents with target type `TARGET_TYPE_LINEAR` as
    /// it implies that the extent is a dm-linear target which are made by concatenating linear
    /// regions (extents) of disk together. For `TARGET_TYPE_ZERO`, this would return [Range(0..0)].
    pub fn used_regions_in_bytes(&self) -> Vec<Range<u64>> {
        self.used_regions.clone().into_iter().map(|r| r.into()).collect()
    }

    // TODO(https://fxbug.dev/404952286): Add support to initialise a partition as a device.
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

#[cfg(test)]
mod tests {
    use crate::{into_merged_regions, SuperDeviceRange, SuperParser};
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
}
