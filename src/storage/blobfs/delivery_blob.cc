// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/blobfs/delivery_blob.h"

#include <lib/cksum.h>
#include <zircon/assert.h>

#include "src/storage/blobfs/delivery_blob_private.h"

#ifndef __APPLE__
#include <endian.h>
#else
#include <machine/endian.h>
#endif
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

bool MetadataType1::IsValid(const DeliveryBlobHeader& header) const {
  return
      // Validate header.
      header.IsValid()
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

}  // namespace blobfs
