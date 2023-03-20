// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <stdio.h>
#include <sys/stat.h>

#include <vector>

#include "blobfs-compression.h"
#include "src/lib/chunked-compression/chunked-compressor.h"
#include "src/lib/chunked-compression/status.h"
#include "src/lib/digest/merkle-tree.h"
#include "src/storage/blobfs/compression/configs/chunked_compression_params.h"
#include "src/storage/blobfs/delivery_blob.h"
#include "src/storage/blobfs/delivery_blob_private.h"
#include "src/storage/blobfs/format.h"

namespace blobfs_compress {
namespace {
using blobfs::DeliveryBlobHeader;
using blobfs::DeliveryBlobType;
using blobfs::MetadataType1;
using chunked_compression::ChunkedCompressor;
using chunked_compression::CompressionParams;

zx::result<std::vector<uint8_t>> GenerateBlobType1(cpp20::span<const uint8_t> data) {
  // WARNING: If we use different compression parameters here, the `compressed_file_size` in the
  // blob info JSON file will be incorrect.
  const CompressionParams params = blobfs::GetDefaultChunkedCompressionParams(data.size_bytes());
  ChunkedCompressor compressor(params);

  constexpr size_t kPayloadOffset = sizeof(DeliveryBlobHeader) + sizeof(MetadataType1);
  const size_t output_limit = params.ComputeOutputSizeLimit(data.size_bytes());
  std::vector<uint8_t> delivery_blob(kPayloadOffset + output_limit);

  size_t compressed_size = 0;
  const chunked_compression::Status status =
      compressor.Compress(data.data(), data.size_bytes(), delivery_blob.data() + kPayloadOffset,
                          output_limit, &compressed_size);
  if (status != chunked_compression::kStatusOk) {
    return zx::error(chunked_compression::ToZxStatus(status));
  }

  const bool use_compressed_result = (compressed_size < data.size_bytes());
  const size_t payload_length = use_compressed_result ? compressed_size : data.size_bytes();
  // Ensure the returned buffer's size reflects the actual payload length.
  delivery_blob.resize(kPayloadOffset + payload_length);
  // Write delivery blob header and metadata.
  const DeliveryBlobHeader header =
      DeliveryBlobHeader::Create(DeliveryBlobType::kType1, sizeof(MetadataType1));
  std::memcpy(delivery_blob.data(), &header, sizeof header);
  const MetadataType1 metadata =
      MetadataType1::Create(header, payload_length, use_compressed_result);
  std::memcpy(delivery_blob.data() + sizeof header, &metadata, sizeof metadata);
  // Overwrite the payload with original data if we aren't compressing the blob.
  if (!use_compressed_result && payload_length > 0) {
    std::memcpy(delivery_blob.data() + kPayloadOffset, data.data(), payload_length);
  }
  return zx::ok(delivery_blob);
}

}  // namespace

// Validate command line |options| used for compressing.
zx_status_t ValidateCliOptions(const CompressionCliOptionStruct& options) {
  if (options.source_file.empty()) {
    return ZX_ERR_INVALID_ARGS;
  }

  // Check source file.
  if (!options.source_file_fd.is_valid()) {
    fprintf(stderr, "Failed to open '%s'.\n", options.source_file.c_str());
    return ZX_ERR_BAD_PATH;
  }
  {
    struct stat info;
    if (fstat(options.source_file_fd.get(), &info) < 0) {
      fprintf(stderr, "stat(%s) failed: %s\n", options.source_file.c_str(), strerror(errno));
      return ZX_ERR_BAD_STATE;
    }
    if (!S_ISREG(info.st_mode)) {
      fprintf(stderr, "%s is not a regular file\n", options.source_file.c_str());
      return ZX_ERR_NOT_FILE;
    }
  }

  // Check compressed output file (can be empty).
  if (!options.compressed_file.empty() && !options.compressed_file_fd.is_valid()) {
    fprintf(stderr, "Failed to open '%s': %s\n", options.compressed_file.c_str(), strerror(errno));
    return ZX_ERR_BAD_PATH;
  }

  return ZX_OK;
}

// Returns 0 if the compression runs successfully; otherwise non-zero values.
// This method reads |src_sz| from |src|, compresses it using the compression
// |params|, and then writes the compressed bytes to |dest_write_buf| and the
// compressed size to |out_compressed_size|. |cli_options| will be used to
// configure what information to include in the output.
//
// |dest_write_buf| can be nullptr if wanting the final compressed size only.
// However, even if |dest_write_buf| is set to nullptr, there will still be
// temporary RAM consumption for storing compressed data due to current internal
// compression API design.
zx_status_t BlobfsCompress(const uint8_t* src, const size_t src_sz, uint8_t* dest_write_buf,
                           size_t* out_compressed_size, CompressionParams params,
                           const CompressionCliOptionStruct& cli_options) {
  ChunkedCompressor compressor(params);

  // Using non-compact merkle tree size by default because it's bigger than compact merkle tree.
  const size_t merkle_tree_size =
      digest::CalculateMerkleTreeSize(src_sz, digest::kDefaultNodeSize, false);
  size_t compressed_size;
  size_t output_limit = params.ComputeOutputSizeLimit(src_sz);
  std::vector<uint8_t> output_buffer;

  // The caller does not need the compressed data. However, the compressor
  // still requires a write buffer to store the compressed output.
  if (dest_write_buf == nullptr) {
    output_buffer.resize(fbl::round_up(output_limit + merkle_tree_size, blobfs::kBlobfsBlockSize));
    dest_write_buf = output_buffer.data();
  }

  const auto compression_status =
      compressor.Compress(src, src_sz, dest_write_buf, output_limit, &compressed_size);
  if (compression_status != chunked_compression::kStatusOk) {
    return chunked_compression::ToZxStatus(compression_status);
  }

  // Final size output should be aligned with block size unless disabled explicitly.
  size_t aligned_source_size = src_sz;
  size_t aligned_compressed_size = compressed_size + merkle_tree_size;
  if (!cli_options.disable_size_alignment) {
    aligned_source_size = fbl::round_up(aligned_source_size, blobfs::kBlobfsBlockSize);
    aligned_compressed_size = fbl::round_up(aligned_compressed_size, blobfs::kBlobfsBlockSize);
  }

  double saving_ratio =
      static_cast<double>(aligned_source_size) - static_cast<double>(aligned_compressed_size);
  if (aligned_source_size) {
    saving_ratio /= static_cast<double>(aligned_source_size);
  } else {
    saving_ratio = 0;
  }

  printf("Wrote %lu bytes (%.2f%% space saved).\n", aligned_compressed_size, saving_ratio * 100);

  // By default, filling 0x00 at the end of compressed buffer to match |aligned_compressed_size|.
  *out_compressed_size = aligned_compressed_size;
  return ZX_OK;
}

zx::result<std::vector<uint8_t>> GenerateDeliveryBlob(cpp20::span<const uint8_t> data,
                                                      DeliveryBlobType type) {
  switch (type) {
    case DeliveryBlobType::kType1:
      return GenerateBlobType1(data);
    default:
      return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
}

}  // namespace blobfs_compress
