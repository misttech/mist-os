// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Library for creating, serializing, and deserializing RFC 0207 delivery blobs. For example, to
//! create a Type 1 delivery blob:
//!
//! ```
//! use delivery_blob::{CompressionMode, Type1Blob};
//! let merkle = "68d131bc271f9c192d4f6dcd8fe61bef90004856da19d0f2f514a7f4098b0737";
//! let data: Vec<u8> = vec![0xFF; 8192];
//! let payload: Vec<u8> = Type1Blob::generate(&data, CompressionMode::Attempt);
//! ```
//!
//! `payload` is now a delivery blob which can be written using the delivery path:
//! ```
//! use delivery_blob::delivery_blob_path;
//! use std::fs::OpenOptions;
//! let path = delivery_blob_path(merkle);
//! let mut file = OpenOptions::new().write(true).create_new(true).open(&path).unwrap();
//! file.set_len(payload.len() as u64).unwrap();
//! file.write_all(&payload).unwrap();

use {
    crate::{
        compression::{ChunkedArchive, ChunkedDecompressor},
        format::SerializedType1Blob,
    },
    serde::{Deserialize, Serialize},
    static_assertions::assert_eq_size,
    thiserror::Error,
    zerocopy::{AsBytes, Ref},
};

#[cfg(target_os = "fuchsia")]
use fuchsia_zircon as zx;

pub mod compression;
mod format;

// This library assumes usize is large enough to hold a u64.
assert_eq_size!(usize, u64);

/// Prefix used for writing delivery blobs. Should be prepended to the Merkle root of the blob.
pub const DELIVERY_PATH_PREFIX: &'static str = "v1-";

/// Generate a delivery blob of the specified `delivery_type` for `data` using default parameters.
pub fn generate(delivery_type: DeliveryBlobType, data: &[u8]) -> Vec<u8> {
    match delivery_type {
        DeliveryBlobType::Type1 => Type1Blob::generate(data, CompressionMode::Attempt),
        _ => panic!("Unsupported delivery blob type: {:?}", delivery_type),
    }
}

/// Generate a delivery blob of the specified `delivery_type` for `data` using default parameters
/// and write the generated blob to `writer`.
pub fn generate_to(
    delivery_type: DeliveryBlobType,
    data: &[u8],
    writer: impl std::io::Write,
) -> Result<(), std::io::Error> {
    match delivery_type {
        DeliveryBlobType::Type1 => Type1Blob::generate_to(data, CompressionMode::Attempt, writer),
        _ => panic!("Unsupported delivery blob type: {:?}", delivery_type),
    }
}

/// Returns the decompressed size of `delivery_blob`, delivery blob type is auto detected.
pub fn decompressed_size(delivery_blob: &[u8]) -> Result<u64, DecompressError> {
    let header = DeliveryBlobHeader::parse(delivery_blob)?.ok_or(DecompressError::NeedMoreData)?;
    match header.delivery_type {
        DeliveryBlobType::Type1 => Type1Blob::decompressed_size(delivery_blob),
        _ => Err(DecompressError::DeliveryBlob(DeliveryBlobError::InvalidType)),
    }
}

/// Returns the decompressed size of the delivery blob from `reader`.
pub fn decompressed_size_from_reader(
    mut reader: impl std::io::Read,
) -> Result<u64, DecompressError> {
    let mut buf = vec![];
    loop {
        let already_read = buf.len();
        let new_size = already_read + 4096;
        buf.resize(new_size, 0);
        let new_size = already_read + reader.read(&mut buf[already_read..new_size])?;
        if new_size == already_read {
            return Err(DecompressError::NeedMoreData);
        }
        buf.truncate(new_size);
        match decompressed_size(&buf) {
            Ok(size) => {
                return Ok(size);
            }
            Err(DecompressError::NeedMoreData) => {}
            Err(e) => {
                return Err(e);
            }
        }
    }
}

/// Decompress a delivery blob in `delivery_blob`, delivery blob type is auto detected.
pub fn decompress(delivery_blob: &[u8]) -> Result<Vec<u8>, DecompressError> {
    let header = DeliveryBlobHeader::parse(delivery_blob)?.ok_or(DecompressError::NeedMoreData)?;
    match header.delivery_type {
        DeliveryBlobType::Type1 => Type1Blob::decompress(delivery_blob),
        _ => Err(DecompressError::DeliveryBlob(DeliveryBlobError::InvalidType)),
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
}

#[derive(Debug, Error)]
pub enum DecompressError {
    #[error("DeliveryBlob error")]
    DeliveryBlob(#[from] DeliveryBlobError),

    #[error("ChunkedArchive error")]
    ChunkedArchive(#[from] compression::ChunkedArchiveError),

    #[error("Need more data")]
    NeedMoreData,

    #[error("io error")]
    IoError(#[from] std::io::Error),
}

#[cfg(target_os = "fuchsia")]
impl From<DeliveryBlobError> for zx::Status {
    fn from(value: DeliveryBlobError) -> Self {
        match value {
            // Unsupported delivery blob type.
            DeliveryBlobError::InvalidType => zx::Status::NOT_SUPPORTED,
            // Potentially corrupted delivery blob.
            DeliveryBlobError::BadMagic | DeliveryBlobError::IntegrityError => {
                zx::Status::IO_DATA_INTEGRITY
            }
        }
    }
}

/// Typed header of an RFC 0207 compliant delivery blob.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeliveryBlobHeader {
    pub delivery_type: DeliveryBlobType,
    pub header_length: u32,
}

impl DeliveryBlobHeader {
    /// Attempt to parse `data` as a delivery blob. On success, returns validated blob header.
    /// **WARNING**: This function does not verify that the payload is complete. Only the full
    /// header of a delivery blob are required to be present in `data`.
    pub fn parse(data: &[u8]) -> Result<Option<DeliveryBlobHeader>, DeliveryBlobError> {
        let Some((serialized_header, _metadata_and_payload)) =
            Ref::<_, format::SerializedHeader>::new_unaligned_from_prefix(data)
        else {
            return Ok(None);
        };
        serialized_header.decode().map(Some)
    }
}

/// Type of delivery blob.
///
/// **WARNING**: These constants are used when generating delivery blobs and should not be changed.
/// Non backwards-compatible changes to delivery blob formats should be made by creating a new type.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
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

impl From<DeliveryBlobType> for u32 {
    fn from(value: DeliveryBlobType) -> Self {
        value as u32
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
    pub const HEADER: DeliveryBlobHeader = DeliveryBlobHeader {
        delivery_type: DeliveryBlobType::Type1,
        header_length: std::mem::size_of::<SerializedType1Blob>() as u32,
    };

    const CHUNK_ALIGNMENT: usize = fuchsia_merkle::BLOCK_SIZE;

    /// Generate a Type 1 delivery blob for `data` using the specified `mode`.
    ///
    /// **WARNING**: This function will panic on error.
    // TODO(https://fxbug.dev/42073034): Bubble up library/compression errors.
    pub fn generate(data: &[u8], mode: CompressionMode) -> Vec<u8> {
        let mut delivery_blob: Vec<u8> = vec![];
        Self::generate_to(data, mode, &mut delivery_blob).unwrap();
        delivery_blob
    }

    /// Generate a Type 1 delivery blob for `data` using the specified `mode`. Writes delivery blob
    /// directly into `writer`.
    ///
    /// **WARNING**: This function will panic on compression errors.
    // TODO(https://fxbug.dev/42073034): Bubble up library/compression errors.
    pub fn generate_to(
        data: &[u8],
        mode: CompressionMode,
        mut writer: impl std::io::Write,
    ) -> Result<(), std::io::Error> {
        // Compress `data` depending on `compression_mode` and if we save any space.
        let compressed = match mode {
            CompressionMode::Attempt | CompressionMode::Always => {
                let compressed = ChunkedArchive::new(data, Self::CHUNK_ALIGNMENT)
                    .expect("failed to compress data");
                if mode == CompressionMode::Always || compressed.serialized_size() <= data.len() {
                    Some(compressed)
                } else {
                    None
                }
            }
            CompressionMode::Never => None,
        };

        // Write header to `writer`.
        let payload_length =
            compressed.as_ref().map(|archive| archive.serialized_size()).unwrap_or(data.len());
        let header =
            Self { header: Type1Blob::HEADER, payload_length, is_compressed: compressed.is_some() };
        let serialized_header: SerializedType1Blob = header.into();
        writer.write_all(serialized_header.as_bytes())?;

        // Write payload to `writer`.
        if let Some(archive) = compressed {
            archive.write(writer)?;
        } else {
            writer.write_all(data)?;
        }
        Ok(())
    }

    /// Attempt to parse `data` as a Type 1 delivery blob. On success, returns validated blob info,
    /// and the remainder of `data` representing the blob payload.
    /// **WARNING**: This function does not verify that the payload is complete. Only the full
    /// header and metadata portion of a delivery blob are required to be present in `data`.
    pub fn parse(data: &[u8]) -> Result<Option<(Type1Blob, &[u8])>, DeliveryBlobError> {
        let Some((serialized_header, payload)) =
            Ref::<_, SerializedType1Blob>::new_unaligned_from_prefix(data)
        else {
            return Ok(None);
        };
        serialized_header.decode().map(|metadata| Some((metadata, payload)))
    }

    /// Return the decompressed size of the blob without decompressing it.
    pub fn decompressed_size(delivery_blob: &[u8]) -> Result<u64, DecompressError> {
        let (header, payload) = Self::parse(delivery_blob)?.ok_or(DecompressError::NeedMoreData)?;
        if !header.is_compressed {
            return Ok(header.payload_length as u64);
        }

        let (seek_table, _chunk_data) =
            compression::decode_archive(payload, header.payload_length)?
                .ok_or(DecompressError::NeedMoreData)?;
        Ok(seek_table.into_iter().map(|chunk| chunk.decompressed_range.len() as u64).sum())
    }

    /// Decompress a Type 1 delivery blob in `delivery_blob`.
    pub fn decompress(delivery_blob: &[u8]) -> Result<Vec<u8>, DecompressError> {
        let (header, payload) = Self::parse(delivery_blob)?.ok_or(DecompressError::NeedMoreData)?;
        if !header.is_compressed {
            return Ok(payload.into());
        }

        let (seek_table, chunk_data) = compression::decode_archive(payload, header.payload_length)?
            .ok_or(DecompressError::NeedMoreData)?;
        let mut decompressor = ChunkedDecompressor::new(seek_table)?;
        let mut decompressed = vec![];
        let mut chunk_callback = |chunk: &[u8]| decompressed.extend_from_slice(chunk);
        decompressor.update(chunk_data, &mut chunk_callback)?;
        Ok(decompressed)
    }
}

#[cfg(test)]
mod tests {

    use {super::*, rand::Rng};

    const DATA_LEN: usize = 500_000;

    #[test]
    fn compression_mode_never() {
        let data: Vec<u8> = vec![0; DATA_LEN];
        let delivery_blob = Type1Blob::generate(&data, CompressionMode::Never);
        // Payload should be uncompressed and have the same size as the original input data.
        let (header, _) = Type1Blob::parse(&delivery_blob).unwrap().unwrap();
        assert!(!header.is_compressed);
        assert_eq!(header.payload_length, data.len());
        assert_eq!(Type1Blob::decompress(&delivery_blob).unwrap(), data);
    }

    #[test]
    fn compression_mode_always() {
        let data: Vec<u8> = {
            let range = rand::distributions::Uniform::<u8>::new_inclusive(0, 255);
            rand::thread_rng().sample_iter(&range).take(DATA_LEN).collect()
        };
        let delivery_blob = Type1Blob::generate(&data, CompressionMode::Always);
        let (header, _) = Type1Blob::parse(&delivery_blob).unwrap().unwrap();
        // Payload is not very compressible, so we expect it to be larger than the original.
        assert!(header.is_compressed);
        assert!(header.payload_length > data.len());
        assert_eq!(Type1Blob::decompress(&delivery_blob).unwrap(), data);
    }

    #[test]
    fn compression_mode_attempt_uncompressible() {
        let data: Vec<u8> = {
            let range = rand::distributions::Uniform::<u8>::new_inclusive(0, 255);
            rand::thread_rng().sample_iter(&range).take(DATA_LEN).collect()
        };
        // Data is random and therefore shouldn't be very compressible.
        let delivery_blob = Type1Blob::generate(&data, CompressionMode::Attempt);
        let (header, _) = Type1Blob::parse(&delivery_blob).unwrap().unwrap();
        assert!(!header.is_compressed);
        assert_eq!(header.payload_length, data.len());
        assert_eq!(Type1Blob::decompress(&delivery_blob).unwrap(), data);
    }

    #[test]
    fn compression_mode_attempt_compressible() {
        let data: Vec<u8> = vec![0; DATA_LEN];
        let delivery_blob = Type1Blob::generate(&data, CompressionMode::Attempt);
        let (header, _) = Type1Blob::parse(&delivery_blob).unwrap().unwrap();
        // Payload should be compressed and smaller than the original input.
        assert!(header.is_compressed);
        assert!(header.payload_length < data.len());
        assert_eq!(Type1Blob::decompress(&delivery_blob).unwrap(), data);
    }

    #[test]
    fn get_decompressed_size() {
        let data: Vec<u8> = {
            let range = rand::distributions::Uniform::<u8>::new_inclusive(0, 255);
            rand::thread_rng().sample_iter(&range).take(DATA_LEN).collect()
        };
        let delivery_blob = Type1Blob::generate(&data, CompressionMode::Always);
        assert_eq!(decompressed_size(&delivery_blob).unwrap(), DATA_LEN as u64);
        assert_eq!(decompressed_size_from_reader(&delivery_blob[..]).unwrap(), DATA_LEN as u64);
    }
}
