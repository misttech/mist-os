// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod format;

use crate::format::{MetadataGeometry, METADATA_GEOMETRY_RESERVED_SIZE, PARTITION_RESERVED_BYTES};
use anyhow::{anyhow, ensure, Error};
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
    // TODO(https://fxbug.dev/404952286): parse Metadata header
}

impl SuperParser {
    pub async fn open_device(device: FakeDevice) -> Result<Self, Error> {
        let metadata_geometry = Self::load_metadata_geometry(&device).await.unwrap();
        let this = Self { metadata_geometry };
        Ok(this)
    }

    async fn load_metadata_geometry(device: &FakeDevice) -> Result<MetadataGeometry, Error> {
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
