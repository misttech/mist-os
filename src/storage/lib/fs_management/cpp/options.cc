// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/fs_management/cpp/options.h"

#include <string>

#include "fidl/fuchsia.fs.startup/cpp/wire_types.h"
#include "lib/fidl/cpp/wire/arena.h"
#include "lib/fidl/cpp/wire/object_view.h"

namespace fs_management {

zx::result<fuchsia_fs_startup::wire::StartOptions> MountOptions::as_start_options(
    fidl::AnyArena &arena) const {
  auto builder = fuchsia_fs_startup::wire::StartOptions::Builder(arena);
  builder.read_only(readonly);
  builder.verbose(verbose_mount);

  if (write_compression_level >= 0) {
    builder.write_compression_level(write_compression_level);
  }

  if (write_compression_algorithm) {
    if (*write_compression_algorithm == "ZSTD_CHUNKED") {
      builder.write_compression_algorithm(
          fuchsia_fs_startup::wire::CompressionAlgorithm::kZstdChunked);
    } else if (*write_compression_algorithm == "UNCOMPRESSED") {
      builder.write_compression_algorithm(
          fuchsia_fs_startup::wire::CompressionAlgorithm::kUncompressed);
    } else {
      return zx::error(ZX_ERR_INVALID_ARGS);
    }
  }

  if (cache_eviction_policy) {
    if (*cache_eviction_policy == "NEVER_EVICT") {
      builder.cache_eviction_policy_override(
          fuchsia_fs_startup::wire::EvictionPolicyOverride::kNeverEvict);
    } else if (*cache_eviction_policy == "EVICT_IMMEDIATELY") {
      builder.cache_eviction_policy_override(
          fuchsia_fs_startup::wire::EvictionPolicyOverride::kEvictImmediately);
    } else if (*cache_eviction_policy == "NONE") {
      builder.cache_eviction_policy_override(
          fuchsia_fs_startup::wire::EvictionPolicyOverride::kNone);
    } else {
      return zx::error(ZX_ERR_INVALID_ARGS);
    }
  }

  if (startup_profiling_seconds) {
    builder.startup_profiling_seconds(*startup_profiling_seconds);
  }
  if (inline_crypto_enabled) {
    builder.inline_crypto_enabled(*inline_crypto_enabled);
  }
  if (barriers_enabled) {
    builder.barriers_enabled(*barriers_enabled);
  }

  return zx::ok(builder.Build());
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
