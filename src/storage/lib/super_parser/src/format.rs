// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, ensure, Error};
use sha2::Digest;
use static_assertions::const_assert;
use zerocopy::{FromBytes, Immutable, IntoBytes};

pub const PARTITION_RESERVED_BYTES: u32 = 4096;
pub const METADATA_GEOMETRY_MAGIC: u32 = 0x616c4467;
pub const METADATA_HEADER_MAGIC: u32 = 0x414C5030;
pub const SECTOR_SIZE: u32 = 512;
pub const METADATA_GEOMETRY_RESERVED_SIZE: u32 = 4096;

pub const METADATA_MAJOR_VERSION: u16 = 10;
pub const METADATA_MINOR_VERSION_MAX: u16 = 2;
pub const CURRENT_METADATA_HEADER_VERSION_MIN: u16 = 2;

/// `MetadataGeometry` provides information on the location of logical partitions. This struct is
/// stored at block 0 of the first 4096 bytes of the partition.
#[repr(C, packed)]
#[derive(Copy, Clone, Debug, FromBytes, IntoBytes, Immutable)]
pub struct MetadataGeometry {
    /// Magic number. Should be `METADATA_GEOMETRY_MAGIC`.
    pub magic: u32,
    /// Size of the metadata geometry size in bytes.
    pub struct_size: u32,
    /// SHA256 checksum of this struct, with this field set to 0.
    pub checksum: [u8; 32],
    /// Maximum size, in bytes, of the metadata. Must be a multiple of `SECTOR_SIZE`.
    pub metadata_max_size: u32,
    /// Number of metadata copies to keep. A backup copy of each slot is kept. So, if the slot
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
            self.struct_size == std::mem::size_of::<MetadataGeometry>() as u32,
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

/// Header of the metadata format. See `MetadataHeaderV1` for the older compatible version.
#[repr(C, packed)]
#[derive(Copy, Clone, Debug, FromBytes, IntoBytes, Immutable)]
pub struct MetadataHeader {
    /// Magic number. Should be `METADATA_HEADER_MAGIC`.
    pub magic: u32,
    /// This struct is versioned. If major version is not METADATA_MAJOR_VERSION, the struct is not
    /// compatible.
    pub major_version: u16,
    /// This struct is versioned. Older minor versions are supported.
    pub minor_version: u16,
    /// Size of the metadata header struct in bytes.
    pub header_size: u32,
    /// SHA256 checksum of this struct, with this field set to zero.
    pub header_checksum: [u8; 32],
    /// Total size of all tables, in bytes.
    pub tables_size: u32,
    /// SHA256 checksum of all table contents.
    pub tables_checksum: [u8; 32],
    /// Partition table descriptor.
    pub partitions: MetadataTableDescriptor,
    /// Extent table descriptor.
    pub extents: MetadataTableDescriptor,
    /// Group table descriptor.
    pub groups: MetadataTableDescriptor,
    /// Block device table descriptor.
    pub block_devices: MetadataTableDescriptor,
    /// Flags are informational. See `HEADER_FLAG_*` constants for possible values. This field is
    /// only found in metadata header version 1.2+.
    // TODO(https://fxbug.dev/404952286): Add `HEADER_FLAG_*` constants.
    pub flags: u32,
    /// Reserved zero, pad to 256 bytes. This is only included in metadata header version 1.2+.
    pub reserved: [u8; 124],
}

/// Size of the older compatible version of metadata header which includes all but `flags` and
/// `reserved` fields of the current `MetadataHeader`. The metadata header is expected to be this
/// size for version 1.0 and 1.1.
pub const METADATA_HEADER_V1_SIZE: usize = std::mem::size_of::<MetadataHeader>()
    - std::mem::size_of::<u32>()
    - std::mem::size_of::<[u8; 124]>();

impl MetadataHeader {
    pub fn compute_checksum(&self) -> [u8; 32] {
        // The `checksum` field is expected to be zero when computing the SHA256 checksum.
        let mut temp_metadata_header = self.clone();
        temp_metadata_header.header_checksum = [0; 32];
        sha2::Sha256::digest(temp_metadata_header.as_bytes()).into()
    }

    pub fn validate(&mut self) -> Result<(), Error> {
        ensure!(self.magic == METADATA_HEADER_MAGIC, "Invalid metadata header magic.");

        ensure!(self.major_version == METADATA_MAJOR_VERSION, "Incompatible metadata version.");
        ensure!(self.minor_version <= METADATA_MINOR_VERSION_MAX, "Incompatible metadata version.");

        if self.minor_version < CURRENT_METADATA_HEADER_VERSION_MIN {
            // Verify size against the old metadata header
            ensure!(
                self.header_size == METADATA_HEADER_V1_SIZE as u32,
                "Incompatible metadata header struct size."
            );
            // If metadata header is the previous version, zero the fields that did not exist.
            self.flags = 0;
            self.reserved = [0; 124];
        } else {
            ensure!(
                self.header_size == std::mem::size_of::<MetadataHeader>() as u32,
                "Incompatible metadata header struct size."
            );
        }

        ensure!(
            self.header_checksum == self.compute_checksum(),
            "Invalid metadata header checksum."
        );

        self.partitions
            .validate_table_bounds(self.tables_size)
            .map_err(|_| anyhow!("partitions tables failed table bounds check."))?;
        self.extents
            .validate_table_bounds(self.tables_size)
            .map_err(|_| anyhow!("extents tables failed table bounds check."))?;
        self.groups
            .validate_table_bounds(self.tables_size)
            .map_err(|_| anyhow!("groups tables failed table bounds check."))?;
        self.block_devices
            .validate_table_bounds(self.tables_size)
            .map_err(|_| anyhow!("block_devices tables failed table bounds check."))?;

        // TODO(https://fxbug.dev/404952286): Validate entry size.

        Ok(())
    }
}

/// These table descriptors are found in `MetadataHeader`.
#[repr(C, packed)]
#[derive(Copy, Clone, Debug, FromBytes, IntoBytes, Immutable)]
pub struct MetadataTableDescriptor {
    /// Location of the table (byte offset relative to the end of the metadata header).
    pub offset: u32,
    /// Number of entries in the table.
    pub num_entries: u32,
    /// Size, in bytes, of each entry in the table. The total size of the table (`num_entries`
    /// multiplied `entry_size`) must not exceed a 32-bit signed integer.
    pub entry_size: u32,
}

impl MetadataTableDescriptor {
    fn validate_table_bounds(&self, total_tables_size: u32) -> Result<(), Error> {
        ensure!(self.offset < total_tables_size, "Invalid table bounds.");
        ensure!(self.num_entries * self.entry_size < total_tables_size, "Invalid table bounds.");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_METADATA_GEOMETRY_BEFORE_COMPUTING_CHECKSUM: MetadataGeometry = MetadataGeometry {
        magic: METADATA_GEOMETRY_MAGIC,
        struct_size: std::mem::size_of::<MetadataGeometry>() as u32,
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

    const VALID_METADATA_HEADER_BEFORE_COMPUTING_CHECKSUM: MetadataHeader = MetadataHeader {
        magic: METADATA_HEADER_MAGIC,
        major_version: METADATA_MAJOR_VERSION,
        minor_version: CURRENT_METADATA_HEADER_VERSION_MIN,
        header_size: std::mem::size_of::<MetadataHeader>() as u32,
        header_checksum: [0; 32],
        tables_size: 188,
        tables_checksum: [0; 32],
        partitions: MetadataTableDescriptor { offset: 0, num_entries: 1, entry_size: 52 },
        extents: MetadataTableDescriptor { offset: 52, num_entries: 1, entry_size: 24 },
        groups: MetadataTableDescriptor { offset: 76, num_entries: 1, entry_size: 48 },
        block_devices: MetadataTableDescriptor { offset: 124, num_entries: 1, entry_size: 64 },
        flags: 1,
        reserved: [0; 124],
    };

    #[fuchsia::test]
    async fn test_valid_metadata_header() {
        let mut header = VALID_METADATA_HEADER_BEFORE_COMPUTING_CHECKSUM;

        // Should fail validation before computing checksum.
        header.validate().expect_err("metadata header passed validation unexpectedly");

        let checksum = header.compute_checksum();
        header.header_checksum = checksum;
        header.validate().expect("metadata header failed validation");
    }

    #[fuchsia::test]
    async fn test_valid_older_metadata_header() {
        let mut header = VALID_METADATA_HEADER_BEFORE_COMPUTING_CHECKSUM;
        header.minor_version = 1;
        assert!(header.minor_version < CURRENT_METADATA_HEADER_VERSION_MIN);
        header.header_size = METADATA_HEADER_V1_SIZE as u32;

        // Zero the fields that does not exist in the older version of the metadata header before
        // calculating its checksum.
        header.flags = 0;
        header.reserved = [0; 124];
        let checksum = header.compute_checksum();
        header.header_checksum = checksum;
        header.validate().expect("metadata header failed validation");
    }

    #[fuchsia::test]
    async fn test_invalid_metadata_header() {
        let mut valid_header = VALID_METADATA_HEADER_BEFORE_COMPUTING_CHECKSUM;
        let checksum = valid_header.compute_checksum();
        valid_header.header_checksum = checksum;
        valid_header.validate().expect("metadata header failed validation");

        // Check that there is validation check for the magic.
        {
            let mut invalid_header = valid_header.clone();
            invalid_header.magic = 1;
            invalid_header.header_checksum = invalid_header.compute_checksum();
            invalid_header
                .validate()
                .expect_err("metadata header with invalid magic passed validation unexpectedly");
        }

        // Check that there is validation check for the major version.
        {
            let mut invalid_header = valid_header.clone();
            invalid_header.major_version = 0;
            invalid_header.header_checksum = invalid_header.compute_checksum();
            invalid_header.validate().expect_err(
                "metadata header with invalid major version passed validation unexpectedly",
            );
        }

        // Check that there is validation check for the minor version.
        {
            let mut invalid_header = valid_header.clone();
            invalid_header.minor_version = METADATA_MINOR_VERSION_MAX + 1;
            invalid_header.header_checksum = invalid_header.compute_checksum();
            invalid_header.validate().expect_err(
                "metadata header with invalid minor version passed validation unexpectedly",
            );
        }

        // Check that there is validation check for the header struct size.
        {
            let mut invalid_header = valid_header.clone();
            invalid_header.header_size = 0;
            invalid_header.header_checksum = invalid_header.compute_checksum();
            invalid_header.validate().expect_err(
                "metadata header with invalid header size passed validation unexpectedly",
            );
        }

        // Check that there is validation check for the table bounds.
        {
            let mut invalid_header = valid_header.clone();
            invalid_header.tables_size = 0;
            invalid_header.header_checksum = invalid_header.compute_checksum();
            invalid_header.validate().expect_err(
                "metadata header with invalid tables size passed validation unexpectedly",
            );
        }

        // Check that there is validation check for the table bounds.
        {
            let mut invalid_header = valid_header.clone();
            invalid_header.block_devices.offset *= 2;
            invalid_header.header_checksum = invalid_header.compute_checksum();
            invalid_header.validate().expect_err(
                "metadata header with invalid entry offset passed validation unexpectedly",
            );
        }
    }
}
