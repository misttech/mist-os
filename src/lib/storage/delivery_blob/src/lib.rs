// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Library for creating, serializing, and deserializing RFC 0207 delivery blobs. For example, to
//! create a Type 1 delivery blob:
//!
//! ```
//! use delivery_blob::Type1Blob;
//! let merkle = "68d131bc271f9c192d4f6dcd8fe61bef90004856da19d0f2f514a7f4098b0737";
//! let data: Vec<u8> = vec![0xFF; 8192];
//! let type_1_blob: Vec<u8> = Type1Blob::generate(&data, None);
//! ```
//!
//! The result represents a delivery blob payload, which can be written as any other blob:
//! ```
//! use delivery_blob::delivery_blob_path;
//! use std::fs::OpenOptions;
//! let path = delivery_blob_path(merkle);
//! let mut file = OpenOptions::new().write(true).create_new(true).open(&path).unwrap();
//! file.set_len(type_1_blob.len() as u64).unwrap();
//! file.write_all(&type_1_blob).unwrap();
//! ```

use {thiserror::Error, zerocopy::AsBytes as _};

use crate::format::SerializedType1Blob;

mod compression;
mod format;

/// Prefix used for writing delivery blobs. Should be prepended to the Merkle root of the blob.
const DELIVERY_PATH_PREFIX: &'static str = "v1-";

/// Generate a delivery blob of the specified `delivery_type` for `data` using default parameters.
pub fn generate(delivery_type: DeliveryBlobType, data: &[u8]) -> Vec<u8> {
    match delivery_type {
        DeliveryBlobType::Type1 => Type1Blob::generate(data, CompressionMode::Attempt),
        _ => panic!("Unsupported delivery blob type: {:?}", delivery_type),
    }
}

/// Obtain the file path to use when writing `blob_name` as a delivery blob.
pub fn delivery_blob_path(blob_name: impl std::fmt::Display) -> String {
    format!("{}{}", DELIVERY_PATH_PREFIX, blob_name)
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum DeliveryBlobError {
    #[error("Invalid or unsupported delivery blob type.")]
    InvalidType,

    #[error("Delivery blob header has incorrect magic.")]
    BadMagic,

    #[error("Integrity/checksum or other validity checks failed.")]
    IntegrityError,

    #[error("Not enough bytes to decode delivery blob.")]
    BufferTooSmall,
}

/// Typed header of an RFC 0207 compliant delivery blob.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeliveryBlobHeader {
    pub delivery_type: DeliveryBlobType,
    pub header_length: usize,
}

/// Type of delivery blob.
///
/// **WARNING**: These constants are used when generating delivery blobs and should not be changed.
/// Non backwards-compatible changes to delivery blob formats should be made by creating a new type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum DeliveryBlobType {
    /// Reserved for internal use.
    Reserved = 0,
    /// Type 1 delivery blobs support the zstd-chunked compression format.
    Type1 = 1,
}

impl TryFrom<u32> for DeliveryBlobType {
    type Error = DeliveryBlobError;
    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            value if value == DeliveryBlobType::Reserved as u32 => Ok(DeliveryBlobType::Reserved),
            value if value == DeliveryBlobType::Type1 as u32 => Ok(DeliveryBlobType::Type1),
            _ => Err(DeliveryBlobError::InvalidType),
        }
    }
}

/// Mode specifying when a delivery blob should be compressed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompressionMode {
    /// Never compress input, output uncompressed.
    Never,
    /// Compress input, output compressed if saves space, otherwise uncompressed.
    Attempt,
    /// Compress input, output compressed unconditionally (even if space is wasted).
    Always,
}

/// Header + metadata fields of a Type 1 blob.
///
/// **WARNING**: Outside of storage-owned components, this should only be used for informational
/// or debugging purposes. The contents of this struct should be considered internal implementation
/// details and are subject to change at any time.
#[derive(Clone, Copy, Debug)]
pub struct Type1Blob {
    // Header:
    pub header: DeliveryBlobHeader,
    // Metadata:
    pub payload_length: usize,
    pub is_compressed: bool,
}

impl Type1Blob {
    const HEADER: DeliveryBlobHeader = DeliveryBlobHeader {
        delivery_type: DeliveryBlobType::Type1,
        header_length: std::mem::size_of::<SerializedType1Blob>(),
    };

    /// Generate a Type 1 delivery blob for `data` using the specified `compression_mode`.
    pub fn generate(data: &[u8], compression_mode: CompressionMode) -> Vec<u8> {
        let compression_buffer: Option<Vec<u8>> = match compression_mode {
            CompressionMode::Attempt | CompressionMode::Always => {
                const CHUNK_ALIGNMENT: usize = fuchsia_merkle::BLOCK_SIZE;
                let compressed =
                    crate::compression::ChunkedArchive::new(data, CHUNK_ALIGNMENT).serialize();
                let use_compressed =
                    compression_mode == CompressionMode::Always || compressed.len() < data.len();
                use_compressed.then_some(compressed)
            }
            CompressionMode::Never => None,
        };
        let payload = compression_buffer.as_ref().map(Vec::as_slice).unwrap_or(data);
        let is_compressed = compression_buffer.is_some();
        let blob_header: SerializedType1Blob =
            Self { header: Type1Blob::HEADER, payload_length: payload.len(), is_compressed }.into();
        [blob_header.as_bytes(), payload].concat()
    }

    /// Attempt to parse `data` as a Type 1 delivery blob. On success, returns validated blob info,
    /// and the remainder of `data` representing the blob payload.
    /// **WARNING**: This function does not verify that the payload is complete. Only the full
    /// header and metadata portion of a delivery blob are required to be present in `data`.
    pub fn parse(data: &[u8]) -> Result<(Type1Blob, &[u8]), DeliveryBlobError> {
        use zerocopy::LayoutVerified;
        let (metadata, payload) =
            LayoutVerified::<_, SerializedType1Blob>::new_unaligned_from_prefix(data)
                .ok_or(DeliveryBlobError::BufferTooSmall)?;
        metadata.decode().map(|metadata| (metadata, payload))
    }
}
