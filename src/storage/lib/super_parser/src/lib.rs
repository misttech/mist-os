// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod format;

use crate::format::{MetadataGeometry, METADATA_GEOMETRY_RESERVED_SIZE, PARTITION_RESERVED_BYTES};
use anyhow::{ensure, Error};
use storage_device::fake_device::FakeDevice;
use storage_device::Device;
use zerocopy::FromBytes;

pub struct SuperParser {
    metadata_geometry: MetadataGeometry,
    // TODO(https://fxbug.dev/404952286): parse Metadata header
}

impl SuperParser {
    pub async fn open_device(device: FakeDevice) -> Result<Self, Error> {
        let metadata_geometry = Self::load_metadata_geometry(&device).await.unwrap();
        let this = Self { metadata_geometry };
        Ok(this)
    }

    async fn load_metadata_geometry(device: &FakeDevice) -> Result<MetadataGeometry, Error> {
        // Reads must be block aligned.
        ensure!(
            METADATA_GEOMETRY_RESERVED_SIZE % device.block_size() == 0,
            "Metadata geometry is not a multiple of device block size."
        );
        let mut buffer = device.allocate_buffer(METADATA_GEOMETRY_RESERVED_SIZE as usize).await;
        device.read(PARTITION_RESERVED_BYTES as u64, buffer.as_mut()).await?;
        let full_geometry_buffer = &buffer.as_slice();
        let (metadata_geometry, _remainder) =
            MetadataGeometry::read_from_prefix(full_geometry_buffer).unwrap();
        Ok(metadata_geometry)
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
    async fn test_parsing_metadata_geometry() {
        let golden = std::path::Path::new("/pkg/data/simple_super.img.zstd");
        let device = open_image(golden);

        let super_partition = SuperParser::open_device(device).await.expect("open device failed");

        let max_size = super_partition.metadata_geometry.metadata_max_size;
        let metadata_slot_count = super_partition.metadata_geometry.metadata_slot_count;
        assert_eq!(max_size, 65536);
        assert_eq!(metadata_slot_count, 1);
    }
}
