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
#include <optional>
#include <string_view>
#include <vector>

#include <fbl/array.h>

namespace blobfs {

constexpr std::string_view kDeliveryBlobPrefix = "v1-";

// Type of the delivery blob's metadata/payload. Corresponds to the `--type` argument used to
// generate the blob with the `blobfs-compression` tool.
//
// *WARNING*: The underlying values of these fields form are used in external tools when generating
// delivery blobs as per RFC 0207. Use caution when changing or re-using the values specified here.
enum class DeliveryBlobType : uint32_t {
  // Reserved for future use.
  kReserved = 0,
  // Type-1 blobs support the zstd-chunked compression format or are uncompressed.
  kType1 = 1,
};

// Header of a delivery blob as specified in RFC 0207.
struct DeliveryBlobHeader {
  // Header Fields:

  // 32-bit magic number.
  uint8_t magic[4];
  // Type of blob the metadata/payload represent.
  DeliveryBlobType type;
  // Total header length, including metadata associated with `type`. For a given delivery blob,
  // the metadata starts after `sizeof(DeliveryBlobHeader)` bytes, and the payload section starts
  // after `header_length` bytes.
  uint32_t header_length;

  // Methods:

  // Check if the header is valid (i.e. `magic` is correct, `type` is a valid value).
  bool IsValid() const;

  // Create a new `DeliveryBlobHeader` with the specified `metadata_length`.
  static DeliveryBlobHeader Create(DeliveryBlobType type, size_t metadata_length);

  // Parse and return a `DeliveryBlobHeader` from a byte `buffer`.
  static zx::result<DeliveryBlobHeader> FromBuffer(cpp20::span<const uint8_t> buffer);
};

// Helper function to prepend the final component of `path` with the prefix specified in RFC 0207.
std::string GetDeliveryBlobPath(const std::string_view& path);

// Generate a Type 1 delivery blob payload from the given blob `data` using the default Blobfs
// compression parameters.
//
// If `compress` is not specified, the result will be uncompressed if the compressed data is larger
// than the input. If `compress` is true, the result will always be compressed, and if false, will
// always be uncompressed.
//
// *WARNING*: Modifying the compression parameters used by this function can cause a mismatch
// between the calculated on-disk size used for size checking. This function is used to calculate
// `compressed_file_size` in the blob info JSON file.
zx::result<fbl::Array<uint8_t>> GenerateDeliveryBlobType1(cpp20::span<const uint8_t> data,
                                                          std::optional<bool> compress);

}  // namespace blobfs

#endif  // SRC_STORAGE_BLOBFS_DELIVERY_BLOB_H_
