// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/blobfs/delivery_blob.h"

#include <lib/cksum.h>
#include <zircon/assert.h>

#include "src/lib/chunked-compression/chunked-decompressor.h"
#include "src/lib/chunked-compression/multithreaded-chunked-compressor.h"
#include "src/lib/digest/merkle-tree.h"
#include "src/storage/blobfs/compression/configs/chunked_compression_params.h"
#include "src/storage/blobfs/delivery_blob_private.h"

#ifndef __APPLE__
#include <endian.h>
#else
#include <machine/endian.h>
#endif
#include <filesystem>
#include <thread>
#include <type_traits>

#include <safemath/safe_conversions.h>

namespace {

constexpr bool IsValidDeliveryType(blobfs::DeliveryBlobType delivery_type) {
  switch (delivery_type) {
    case blobfs::DeliveryBlobType::kType1:
      return true;
    default:
      return false;
  }
}

zx::result<fbl::Array<uint8_t>> CompressData(cpp20::span<const uint8_t> data,
                                             const chunked_compression::CompressionParams& params) {
  const size_t num_chunks = fbl::round_up(data.size_bytes(), params.chunk_size) / params.chunk_size;
  // If `data` spans multiple chunks, we compress each chunk in parallel.
  if (num_chunks > 1) {
    const size_t num_threads = std::min<size_t>(num_chunks, std::thread::hardware_concurrency());
    chunked_compression::MultithreadedChunkedCompressor compressor(num_threads);
    return compressor.Compress(params, data);
  }

  chunked_compression::ChunkedCompressor compressor(params);
  const size_t output_limit = params.ComputeOutputSizeLimit(data.size_bytes());
  fbl::Array<uint8_t> compressed_result = fbl::MakeArray<uint8_t>(output_limit);
  size_t compressed_size;
  const chunked_compression::Status status = compressor.Compress(
      data.data(), data.size_bytes(), compressed_result.data(), output_limit, &compressed_size);
  if (status != chunked_compression::kStatusOk) {
    return zx::error(chunked_compression::ToZxStatus(status));
  }
  return zx::ok(fbl::Array<uint8_t>(compressed_result.release(), compressed_size));
}

}  // namespace

namespace blobfs {

// Format layout assumptions.
// **WARNING**: Use caution when updating these assertions as it may break format compatibility.
static_assert(__BYTE_ORDER__ == __ORDER_LITTLE_ENDIAN__);
static_assert(sizeof(DeliveryBlobHeader) == 12);
static_assert(std::is_trivial_v<DeliveryBlobHeader>);
static_assert(sizeof(MetadataType1) == 16);
static_assert(std::is_trivial_v<MetadataType1>);

/// Delivery blob magic number (0xfc1ab10b or "Fuchsia Blob") in big-endian.
constexpr uint8_t kDeliveryBlobMagic[4] = {0xfc, 0x1a, 0xb1, 0x0b};

DeliveryBlobHeader DeliveryBlobHeader::Create(DeliveryBlobType type, size_t metadata_length) {
  return {.magic = {kDeliveryBlobMagic[0], kDeliveryBlobMagic[1], kDeliveryBlobMagic[2],
                    kDeliveryBlobMagic[3]},
          .type = type,
          .header_length =
              safemath::checked_cast<uint32_t>(sizeof(DeliveryBlobHeader) + metadata_length)};
}

bool DeliveryBlobHeader::IsValid() const {
  return std::equal(std::begin(magic), std::end(magic), std::begin(kDeliveryBlobMagic)) &&
         IsValidDeliveryType(type);
};

zx::result<DeliveryBlobHeader> DeliveryBlobHeader::FromBuffer(cpp20::span<const uint8_t> buffer) {
  if (buffer.size_bytes() < sizeof(DeliveryBlobHeader)) {
    return zx::error(ZX_ERR_BUFFER_TOO_SMALL);
  }
  // We cannot guarantee `buffer` is aligned, so we must use `memcpy`.
  DeliveryBlobHeader header = {};
  std::memcpy(&header, buffer.data(), sizeof(DeliveryBlobHeader));
  if (!header.IsValid()) {
    return zx::error(ZX_ERR_IO_DATA_INTEGRITY);
  }
  return zx::ok(header);
}

const DeliveryBlobHeader MetadataType1::kHeader =
    DeliveryBlobHeader::Create(DeliveryBlobType::kType1, sizeof(MetadataType1));

bool MetadataType1::IsValid(const DeliveryBlobHeader& header) const {
  constexpr size_t kExpectedHeaderLength = sizeof(DeliveryBlobHeader) + sizeof(MetadataType1);
  return
      // Validate header.
      header.IsValid()
      // Validate header length.
      && (header.header_length == kExpectedHeaderLength)
      // Validate CRC.
      && (checksum == Checksum(header))
      // Validate flags.
      && ((flags & ~kValidFlagsMask) == 0);
}

uint32_t MetadataType1::Checksum(const DeliveryBlobHeader& header) const {
  const uint32_t header_crc =
      ::crc32(0, reinterpret_cast<const uint8_t*>(&header), sizeof(blobfs::DeliveryBlobHeader));
  // Make a copy of `metadata` so we can zero the checksum field before the calculation.
  MetadataType1 metadata_copy = *this;
  metadata_copy.checksum = 0;
  const uint32_t metadata_crc =
      ::crc32(0, reinterpret_cast<const uint8_t*>(&metadata_copy), sizeof(MetadataType1));
  return crc32_combine(header_crc, metadata_crc, sizeof(MetadataType1));
}

MetadataType1 MetadataType1::Create(const DeliveryBlobHeader& header, size_t payload_length,
                                    bool is_compressed) {
  MetadataType1 metadata = {
      .payload_length = safemath::checked_cast<uint64_t>(payload_length),
      .checksum = {},
      .flags = (is_compressed ? kIsCompressed : 0),
  };
  metadata.checksum = metadata.Checksum(header);
  ZX_ASSERT(metadata.IsValid(header));
  return metadata;
}

zx::result<MetadataType1> MetadataType1::FromBuffer(cpp20::span<const uint8_t> buffer,
                                                    const blobfs::DeliveryBlobHeader& header) {
  if (buffer.size_bytes() < sizeof(MetadataType1)) {
    return zx::error(ZX_ERR_BUFFER_TOO_SMALL);
  }
  // We cannot guarantee `buffer` is aligned, so we must use `memcpy`.
  MetadataType1 metadata = {};
  std::memcpy(&metadata, buffer.data(), sizeof(MetadataType1));
  // Ensure the parsed metadata is valid (i.e. checksums or other sanity checks pass).
  if (!metadata.IsValid(header)) {
    return zx::error(ZX_ERR_IO_DATA_INTEGRITY);
  }
  return zx::ok(metadata);
}

std::string GetDeliveryBlobPath(const std::string_view& path) {
  std::filesystem::path fs_path(path);
  const std::string new_filename = std::string(kDeliveryBlobPrefix) + fs_path.filename().string();
  return fs_path.replace_filename(new_filename);
}

zx::result<fbl::Array<uint8_t>> GenerateDeliveryBlobType1(cpp20::span<const uint8_t> data,
                                                          std::optional<bool> compress) {
  constexpr size_t kPayloadOffset = sizeof(DeliveryBlobHeader) + sizeof(MetadataType1);

  fbl::Array<uint8_t> delivery_blob;

  if (compress.value_or(true)) {
    // WARNING: If we use different compression parameters here, the `compressed_file_size` in the
    // blob info JSON file will be incorrect.
    const chunked_compression::CompressionParams params =
        blobfs::GetDefaultChunkedCompressionParams(data.size_bytes());
    zx::result compressed_data = CompressData(data, params);
    if (compressed_data.is_error()) {
      return compressed_data.take_error();
    }
    // Only use the compressed result if it saves space or we require the payload to be compressed.
    if (compressed_data->size() < data.size_bytes() || compress.value_or(false)) {
      delivery_blob = fbl::MakeArray<uint8_t>(kPayloadOffset + compressed_data->size());
      std::memcpy(delivery_blob.data() + kPayloadOffset, compressed_data->data(),
                  compressed_data->size());
    }
  }
  const bool use_compressed_result = !delivery_blob.empty();
  if (!use_compressed_result) {
    delivery_blob = fbl::MakeArray<uint8_t>(kPayloadOffset + data.size_bytes());
  }
  // Write delivery blob header and metadata.
  std::memcpy(delivery_blob.data(), &MetadataType1::kHeader, sizeof MetadataType1::kHeader);
  const MetadataType1 metadata =
      MetadataType1::Create(MetadataType1::kHeader, delivery_blob.size() - kPayloadOffset,
                            /*is_compressed=*/use_compressed_result);
  std::memcpy(delivery_blob.data() + sizeof MetadataType1::kHeader, &metadata, sizeof metadata);
  // Copy uncompressed data into payload if we aren't using compression or we aborted compression.
  if (!use_compressed_result && !data.empty()) {
    std::memcpy(delivery_blob.data() + kPayloadOffset, data.data(), data.size_bytes());
  }
  return zx::ok(std::move(delivery_blob));
}

zx::result<digest::Digest> CalculateDeliveryBlobDigest(cpp20::span<const uint8_t> data) {
  zx::result header = DeliveryBlobHeader::FromBuffer(data);
  if (header.is_error()) {
    return header.take_error();
  }
  if (header->type != DeliveryBlobType::kType1) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  // Currently, Type 1 blobs have a fixed header size, but this may change if we move the seek table
  // from the payload section into the metadata.
  constexpr size_t kExpectedHeaderLength = sizeof(DeliveryBlobHeader) + sizeof(MetadataType1);
  if (data.size() < kExpectedHeaderLength) {
    return zx::error(ZX_ERR_BUFFER_TOO_SMALL);
  }
  if (header->header_length != kExpectedHeaderLength) {
    return zx::error(ZX_ERR_IO_DATA_INTEGRITY);
  }

  zx::result metadata =
      MetadataType1::FromBuffer(data.subspan(sizeof(DeliveryBlobHeader)), *header);
  if (metadata.is_error()) {
    return metadata.take_error();
  }

  cpp20::span<const uint8_t> payload = data.subspan(header->header_length);
  if (payload.size() != metadata->payload_length) {
    return zx::error(ZX_ERR_IO_DATA_INTEGRITY);
  }

  // Decompress `payload` if required, and update the span to point to the uncompressed result.
  fbl::Array<uint8_t> decompressed_result;
  if (metadata->IsCompressed() && !payload.empty()) {
    size_t unused_bytes_written;
    if (chunked_compression::Status status =
            chunked_compression::ChunkedDecompressor::DecompressBytes(
                payload.data(), payload.size(), &decompressed_result, &unused_bytes_written);
        status != chunked_compression::kStatusOk) {
      return zx::error(chunked_compression::ToZxStatus(status));
    }
    payload = decompressed_result;
  }

  // Calculate Merkle root of `payload` which points to the uncompressed blob data (may be empty).
  std::unique_ptr<uint8_t[]> unused_tree;
  size_t unused_tree_len;
  digest::Digest root;

  if (zx_status_t status = digest::MerkleTreeCreator::Create(payload.data(), payload.size(),
                                                             &unused_tree, &unused_tree_len, &root);
      status != ZX_OK) {
    return zx::error(status);
  }

  return zx::ok(root);
}

}  // namespace blobfs
