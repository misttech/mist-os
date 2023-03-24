// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <array>

#include <gtest/gtest.h>

#include "src/lib/chunked-compression/chunked-decompressor.h"
#include "src/storage/blobfs/delivery_blob.h"
#include "src/storage/blobfs/delivery_blob_private.h"
#include "src/storage/tools/blobfs-compression/blobfs-compression.h"

namespace blobfs_compress {

using namespace blobfs;

TEST(OfflineCompressionTest, Type1EmptyBlob) {
  const zx::result<fbl::Array<uint8_t>> delivery_blob =
      GenerateDeliveryBlob({}, DeliveryBlobType::kType1);
  ASSERT_TRUE(delivery_blob.is_ok());
  // Data should only contain headers and no payload.
  ASSERT_EQ(delivery_blob->size(), sizeof(DeliveryBlobHeader) + sizeof(MetadataType1));

  // Decode headers/metadata and validate.
  const cpp20::span<const uint8_t> buffer(delivery_blob->data(), delivery_blob->size());
  const zx::result<DeliveryBlobHeader> header = DeliveryBlobHeader::FromBuffer(buffer);
  ASSERT_TRUE(header.is_ok());
  ASSERT_TRUE(header->IsValid());
  ASSERT_EQ(header->type, DeliveryBlobType::kType1);
  EXPECT_EQ(header->header_length, sizeof(DeliveryBlobHeader) + sizeof(MetadataType1));

  const zx::result<MetadataType1> metadata =
      MetadataType1::FromBuffer(buffer.subspan(sizeof(DeliveryBlobHeader)), *header);
  ASSERT_TRUE(metadata.is_ok());
  ASSERT_TRUE(metadata->IsValid(*header));
  EXPECT_EQ(metadata->payload_length, 0u);
  EXPECT_EQ(metadata->flags & ~MetadataType1::kValidFlagsMask, 0u);
  // There should be no payload to decompress, so the IsCompressed bit should be zero.
  EXPECT_FALSE(metadata->flags & MetadataType1::kIsCompressed);
}

TEST(OfflineCompressionTest, Type1ShouldNotCompress) {
  // If we have less data to compress than the zstd-chunked header, we shouldn't use compression
  // as the result would always be larger than the original.
  constexpr size_t kSmallPayloadSize = 4u;
  const std::vector<uint8_t> blob_data(kSmallPayloadSize);

  const zx::result<fbl::Array<uint8_t>> delivery_blob =
      GenerateDeliveryBlob({blob_data.data(), blob_data.size()}, DeliveryBlobType::kType1);
  ASSERT_TRUE(delivery_blob.is_ok());
  ASSERT_EQ(delivery_blob->size(),
            sizeof(DeliveryBlobHeader) + sizeof(MetadataType1) + blob_data.size());

  // Decode headers/metadata and validate.
  const cpp20::span buffer(delivery_blob->data(), delivery_blob->size());
  const zx::result<DeliveryBlobHeader> header = DeliveryBlobHeader::FromBuffer(buffer);
  ASSERT_TRUE(header.is_ok());
  ASSERT_TRUE(header->IsValid());
  ASSERT_EQ(header->type, DeliveryBlobType::kType1);
  EXPECT_EQ(header->header_length, sizeof(DeliveryBlobHeader) + sizeof(MetadataType1));

  const cpp20::span metadata_buffer = buffer.subspan(sizeof(DeliveryBlobHeader));
  const zx::result<MetadataType1> metadata = MetadataType1::FromBuffer(metadata_buffer, *header);
  ASSERT_TRUE(metadata.is_ok());
  ASSERT_TRUE(metadata->IsValid(*header));
  EXPECT_EQ(metadata->payload_length, blob_data.size());

  // Validate payload itself.
  const cpp20::span payload_buffer = metadata_buffer.subspan(sizeof(MetadataType1));
  ASSERT_EQ(payload_buffer.size(), blob_data.size());
  ASSERT_TRUE(std::equal(payload_buffer.begin(), payload_buffer.end(), blob_data.cbegin()));
}

TEST(OfflineCompressionTest, Type1ShouldCompress) {
  constexpr size_t kLargePayloadSize = 1ul << 16;
  const std::vector<uint8_t> blob_data(kLargePayloadSize);

  const zx::result<fbl::Array<uint8_t>> delivery_blob =
      GenerateDeliveryBlob({blob_data.data(), blob_data.size()}, DeliveryBlobType::kType1);
  ASSERT_TRUE(delivery_blob.is_ok());
  ASSERT_LE(delivery_blob->size(),
            sizeof(DeliveryBlobHeader) + sizeof(MetadataType1) + blob_data.size());

  // Decode headers/metadata and validate.
  const cpp20::span buffer(delivery_blob->data(), delivery_blob->size());
  const zx::result<DeliveryBlobHeader> header = DeliveryBlobHeader::FromBuffer(buffer);
  ASSERT_TRUE(header.is_ok());
  ASSERT_TRUE(header->IsValid());
  ASSERT_EQ(header->type, DeliveryBlobType::kType1);
  EXPECT_EQ(header->header_length, sizeof(DeliveryBlobHeader) + sizeof(MetadataType1));

  const cpp20::span metadata_buffer = buffer.subspan(sizeof(DeliveryBlobHeader));
  const zx::result<MetadataType1> metadata = MetadataType1::FromBuffer(metadata_buffer, *header);
  ASSERT_TRUE(metadata.is_ok());
  ASSERT_TRUE(metadata->IsValid(*header));
  // The payload length should be less than the actual data as it should be compressed.
  EXPECT_LE(metadata->payload_length, blob_data.size());
  EXPECT_TRUE((metadata->flags & MetadataType1::kValidFlagsMask) != 0u);

  // Ensure that we can correctly decompress the payload.
  const cpp20::span payload_buffer = metadata_buffer.subspan(sizeof(MetadataType1));
  fbl::Array<uint8_t> decompressed_result;
  size_t unused;
  ASSERT_EQ(chunked_compression::ChunkedDecompressor::DecompressBytes(
                payload_buffer.data(), payload_buffer.size(), &decompressed_result, &unused),
            chunked_compression::kStatusOk);
  // Verify the decompressed result.
  ASSERT_EQ(decompressed_result.size(), blob_data.size());
  ASSERT_TRUE(
      std::equal(decompressed_result.begin(), decompressed_result.end(), blob_data.cbegin()));
}

}  // namespace blobfs_compress
