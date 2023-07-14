// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/lib/storage/fs_management/cpp/options.h"

#include <string>

#include "fidl/fuchsia.fs.startup/cpp/wire_types.h"

namespace fs_management {

zx::result<fuchsia_fs_startup::wire::StartOptions> MountOptions::as_start_options() const {
  fuchsia_fs_startup::wire::StartOptions options;

  options.read_only = readonly;
  options.verbose = verbose_mount;
  options.write_compression_level = write_compression_level;

  if (write_compression_algorithm) {
    if (*write_compression_algorithm == "ZSTD_CHUNKED") {
      options.write_compression_algorithm =
          fuchsia_fs_startup::wire::CompressionAlgorithm::kZstdChunked;
    } else if (*write_compression_algorithm == "UNCOMPRESSED") {
      options.write_compression_algorithm =
          fuchsia_fs_startup::wire::CompressionAlgorithm::kUncompressed;
    } else {
      return zx::error(ZX_ERR_INVALID_ARGS);
    }
  } else {
    options.write_compression_algorithm =
        fuchsia_fs_startup::wire::CompressionAlgorithm::kZstdChunked;
  }

  if (cache_eviction_policy) {
    if (*cache_eviction_policy == "NEVER_EVICT") {
      options.cache_eviction_policy_override =
          fuchsia_fs_startup::wire::EvictionPolicyOverride::kNeverEvict;
    } else if (*cache_eviction_policy == "EVICT_IMMEDIATELY") {
      options.cache_eviction_policy_override =
          fuchsia_fs_startup::wire::EvictionPolicyOverride::kEvictImmediately;
    } else if (*cache_eviction_policy == "NONE") {
      options.cache_eviction_policy_override =
          fuchsia_fs_startup::wire::EvictionPolicyOverride::kNone;
    } else {
      return zx::error(ZX_ERR_INVALID_ARGS);
    }
  } else {
    options.cache_eviction_policy_override =
        fuchsia_fs_startup::wire::EvictionPolicyOverride::kNone;
  }

  options.allow_delivery_blobs = allow_delivery_blobs;

  return zx::ok(options);
}

fuchsia_fs_startup::wire::FormatOptions MkfsOptions::as_format_options(
    fidl::AnyArena &arena) const {
  auto builder = fuchsia_fs_startup::wire::FormatOptions::Builder(arena);
  builder.verbose(verbose);
  builder.deprecated_padded_blobfs_format(deprecated_padded_blobfs_format);
  if (num_inodes > 0)
    builder.num_inodes(num_inodes);
  if (fvm_data_slices > 0)
    builder.fvm_data_slices(fvm_data_slices);
  if (sectors_per_cluster > 0)
    builder.sectors_per_cluster(sectors_per_cluster);
  return builder.Build();
}

fuchsia_fs_startup::wire::CheckOptions FsckOptions::as_check_options() const {
  return fuchsia_fs_startup::wire::CheckOptions{};
}

}  // namespace fs_management
