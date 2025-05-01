// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{ensure, Error};
use sha2::Digest;
use static_assertions::const_assert;
use zerocopy::{FromBytes, Immutable, IntoBytes};

pub const PARTITION_RESERVED_BYTES: u32 = 4096;
pub const METADATA_GEOMETRY_MAGIC: u32 = 0x616c4467;
pub const SECTOR_SIZE: u32 = 512;
pub const METADATA_GEOMETRY_RESERVED_SIZE: u32 = 4096;

/// `MetadataGeometry` provides information on the location of logical partitions. This struct is
/// stored at block 0 of the first 4096 bytes of the partition.
#[repr(C, packed)]
#[derive(Copy, Clone, Debug, FromBytes, IntoBytes, Immutable)]
pub struct MetadataGeometry {
    /// Magic number. Should be METADATA_GEOMETRY_MAGIC.
    pub magic: u32,
    /// Size of the metadata geometry size in bytes. Should be 4096 bytes.
    pub struct_size: u32,
    /// SHA256 checksum of this struct, with this field set to 0.
    pub checksum: [u8; 32],
    /// Maximum size, in bytes, of `Metadata`. Must be a multiple of `SECTOR_SIZE`.
    pub metadata_max_size: u32,
    /// Number of `Metadata` copies to keep. A backup copy of each slot is kept. So, if the slot
    /// count is "2", there will be a total of four copies.
    pub metadata_slot_count: u32,
    /// The logical block size in bytes. Must be a multiple of `SECTOR_SIZE`.
    pub logical_block_size: u32,
}
const_assert!(std::mem::size_of::<MetadataGeometry>() as u32 <= METADATA_GEOMETRY_RESERVED_SIZE);

impl MetadataGeometry {
    pub fn compute_checksum(&self) -> [u8; 32] {
        // The `checksum` field is expected to be zero when computing the SHA256 checksum.
        let mut temp_metadata_geometry = self.clone();
        temp_metadata_geometry.checksum = [0; 32];
        sha2::Sha256::digest(temp_metadata_geometry.as_bytes()).into()
    }

    pub fn validate(&self) -> Result<(), Error> {
        ensure!(self.magic == METADATA_GEOMETRY_MAGIC, "Invalid metadata geometry magic.");
        ensure!(
            self.struct_size == METADATA_GEOMETRY_RESERVED_SIZE,
            "Invalid metadata geometry struct size."
        );
        ensure!(self.checksum == self.compute_checksum(), "Invalid metadata geometry checksum.");
        ensure!(self.metadata_slot_count > 0, "Invalid metadata slot count. Must be more than 0.");
        ensure!(
            self.metadata_max_size % SECTOR_SIZE == 0,
            "Invalid metadata maximum size. Must be sector-aligned."
        );
        ensure!(
            self.logical_block_size % SECTOR_SIZE == 0,
            "Invalid logical block size. Must be sector-aligned."
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_METADATA_GEOMETRY_BEFORE_COMPUTING_CHECKSUM: MetadataGeometry = MetadataGeometry {
        magic: METADATA_GEOMETRY_MAGIC,
        struct_size: METADATA_GEOMETRY_RESERVED_SIZE,
        checksum: [0; 32],
        metadata_max_size: 6 * SECTOR_SIZE,
        metadata_slot_count: 2,
        logical_block_size: 6 * SECTOR_SIZE,
    };

    #[fuchsia::test]
    async fn test_valid_metadata_geometry() {
        let mut geometry = VALID_METADATA_GEOMETRY_BEFORE_COMPUTING_CHECKSUM;

        // Should fail validation before computing checksum.
        geometry.validate().expect_err("metadata geometry passed validation unexpectedly");

        let checksum = geometry.compute_checksum();
        geometry.checksum = checksum;
        geometry.validate().expect("metadata geometry failed validation");
    }

    #[fuchsia::test]
    async fn test_invalid_metadata_geometry_magic() {
        let mut geometry = VALID_METADATA_GEOMETRY_BEFORE_COMPUTING_CHECKSUM;
        geometry.magic = 1;
        let checksum = geometry.compute_checksum();
        geometry.checksum = checksum;
        geometry.validate().expect_err("metadata geometry passed validation unexpectedly");
    }

    #[fuchsia::test]
    async fn test_invalid_metadata_geometry_struct_size() {
        let mut geometry = VALID_METADATA_GEOMETRY_BEFORE_COMPUTING_CHECKSUM;
        geometry.struct_size = 8192;
        let checksum = geometry.compute_checksum();
        geometry.checksum = checksum;
        geometry.validate().expect_err("metadata geometry passed validation unexpectedly");
    }

    #[fuchsia::test]
    async fn test_invalid_metadata_geometry_metadata_max_size() {
        let mut geometry = VALID_METADATA_GEOMETRY_BEFORE_COMPUTING_CHECKSUM;
        geometry.metadata_max_size = geometry.metadata_max_size - 1;
        let checksum = geometry.compute_checksum();
        geometry.checksum = checksum;
        geometry.validate().expect_err("metadata geometry passed validation unexpectedly");
    }

    #[fuchsia::test]
    async fn test_invalid_metadata_geometry_metadata_slot_count() {
        let mut geometry = VALID_METADATA_GEOMETRY_BEFORE_COMPUTING_CHECKSUM;
        geometry.metadata_slot_count = 0;
        let checksum = geometry.compute_checksum();
        geometry.checksum = checksum;
        geometry.validate().expect_err("metadata geometry passed validation unexpectedly");
    }

    #[fuchsia::test]
    async fn test_invalid_metadata_geometry_logical_block_size() {
        let mut geometry = VALID_METADATA_GEOMETRY_BEFORE_COMPUTING_CHECKSUM;
        geometry.logical_block_size = geometry.logical_block_size - 1;
        let checksum = geometry.compute_checksum();
        geometry.checksum = checksum;
        geometry.validate().expect_err("metadata geometry passed validation unexpectedly");
    }
}
