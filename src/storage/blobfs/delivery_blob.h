// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This file describes the format of delivery blobs as specified in RFC 0207.
// It includes high-level types for interfacing with a delivery blob.

#ifndef SRC_STORAGE_BLOBFS_DELIVERY_BLOB_H_
#define SRC_STORAGE_BLOBFS_DELIVERY_BLOB_H_

#include <lib/stdcompat/span.h>
#include <lib/zx/result.h>
#include <zircon/compiler.h>

#include <cstdint>
#include <cstdlib>

namespace blobfs {

/// Type of the delivery blob's metadata/payload. Corresponds to the `--type` argument used to
/// generate the blob with the `blobfs-compression` tool.
///
/// *WARNING*: The underlying values of these fields form are used in external tools when generating
/// delivery blobs as per RFC 0207. Use caution when changing or re-using the values specified here.
enum class DeliveryBlobType : uint32_t {
  kUncompressed = 0,
  kZstdChunked = 1,
};

/// Header of a delivery blob as specified in RFC 0207.
struct DeliveryBlobHeader {
  // Header Fields:

  /// 32-bit magic number.
  uint8_t magic[4];
  /// Type of blob the metadata/payload represent.
  DeliveryBlobType type;
  /// Total header length, including metadata associated with `type`. For a given delivery blob,
  /// the metadata starts after `sizeof(DeliveryBlobHeader)` bytes, and the payload section starts
  /// after `header_length` bytes.
  uint32_t header_length;

  // Methods:

  /// Check if the header is valid (i.e. `magic` is correct, `type` is a valid value).
  bool IsValid() const;

  /// Create a new `DeliveryBlobHeader` with the specified `metadata_length`.
  static DeliveryBlobHeader Create(DeliveryBlobType type, size_t metadata_length);

  /// Parse and return a `DeliveryBlobHeader` from a byte `buffer`.
  static zx::result<DeliveryBlobHeader> FromBuffer(cpp20::span<const uint8_t> buffer);
};

/// Metadata format corresponding to a delivery blob of type `DeliveryBlobType::kUncompressed`.
struct UncompressedMetadata {
  // Uncompressed Metadata Fields:

  /// [RESERVED] Should always be 0 currently.
  uint32_t reserved;
  /// CRC32 covering both header + metadata. The field itself should be zero when calculating.
  uint32_t crc32;
  /// Length of uncompressed blob data (payload), in bytes.
  uint64_t payload_length;

  // Methods:

  /// Check if the metadata is valid. Requires `header` for checksum validation.
  bool IsValid(const DeliveryBlobHeader& header) const;

  /// Calculate the checksum over the header and this metadata.
  uint32_t Checksum(const DeliveryBlobHeader& header) const;

  /// Create a new `UncompressedMetadata` with the specified `payload_length`.
  static UncompressedMetadata Create(const DeliveryBlobHeader& header, size_t payload_length);

  /// Parse and return an `UncompressedMetadata` from a byte `buffer` and a parsed `header`.
  static zx::result<UncompressedMetadata> FromBuffer(cpp20::span<const uint8_t> buffer,
                                                     const blobfs::DeliveryBlobHeader& header);
};

/// Metadata format corresponding to a delivery blob of type `DeliveryBlobType::kZstdChunked`.
struct ZstdChunkedMetadata {
  using FlagsType = uint32_t;
  /// Flag indicating if the payload is compressed with the zstd-chunked format. If not set, the
  /// payload is assumed to be an uncompressed blob.
  static constexpr FlagsType kIsCompressed = 1ul << 0;
  /// Mask of all valid flags.
  static constexpr FlagsType kValidFlagsMask = kIsCompressed;

  // Zstd-Chunked Metadata Fields:

  /// [RESERVED] Should always be 0 currently.
  uint32_t reserved;
  /// CRC32 covering both header + metadata. The field itself should be zero when calculating.
  uint32_t crc32;
  /// Expected length of payload, in bytes. May be compressed or uncompressed depending on `flags`.
  uint64_t payload_length;
  /// Flags indicating format and how to decode payload.
  FlagsType flags;

  /// Check if the metadata is valid. Requires `header` for checksum validation.
  bool IsValid(const DeliveryBlobHeader& header) const;

  /// Calculate the checksum over the header and this metadata.
  uint32_t Checksum(const DeliveryBlobHeader& header) const;

  /// Create a new `ZstdChunkedMetadata` with the specified `payload_length`. If `is_compressed` is
  /// true, the payload is assumed to be in the zstd-chunked format. Otherwise, the payload
  /// following the metadata ia assumed to be uncompressed.
  static ZstdChunkedMetadata Create(const DeliveryBlobHeader& header, size_t payload_length,
                                    bool is_compressed);

  /// Parse and return a `ZstdChunkedMetadata` from a byte `buffer` and a parsed `header`.
  static zx::result<ZstdChunkedMetadata> FromBuffer(cpp20::span<const uint8_t> buffer,
                                                    const blobfs::DeliveryBlobHeader& header);
};

}  // namespace blobfs

#endif  // SRC_STORAGE_BLOBFS_DELIVERY_BLOB_H_
