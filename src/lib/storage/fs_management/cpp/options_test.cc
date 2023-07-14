// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/lib/storage/fs_management/cpp/options.h"

#include <gtest/gtest.h>

namespace fs_management {

namespace {

const std::string kTestBinary = "/test/binary";

void AssertStartOptionsEqual(const fuchsia_fs_startup::wire::StartOptions& a,
                             const fuchsia_fs_startup::wire::StartOptions& b) {
  ASSERT_EQ(a.read_only, b.read_only);
  ASSERT_EQ(a.verbose, b.verbose);
  ASSERT_EQ(a.write_compression_algorithm, b.write_compression_algorithm);
  ASSERT_EQ(a.write_compression_level, b.write_compression_level);
  ASSERT_EQ(a.cache_eviction_policy_override, b.cache_eviction_policy_override);
}

void AssertFormatOptionsEqual(const fuchsia_fs_startup::wire::FormatOptions& a,
                              const fuchsia_fs_startup::wire::FormatOptions& b) {
  ASSERT_EQ(a.has_verbose(), b.has_verbose());
  if (a.has_verbose())
    ASSERT_EQ(a.verbose(), b.verbose());
  ASSERT_EQ(a.has_num_inodes(), b.has_num_inodes());
  if (a.has_num_inodes())
    ASSERT_EQ(a.num_inodes(), b.num_inodes());
  ASSERT_EQ(a.has_deprecated_padded_blobfs_format(), b.has_deprecated_padded_blobfs_format());
  if (a.has_deprecated_padded_blobfs_format())
    ASSERT_EQ(a.deprecated_padded_blobfs_format(), b.deprecated_padded_blobfs_format());
  ASSERT_EQ(a.has_fvm_data_slices(), b.has_fvm_data_slices());
  if (a.has_fvm_data_slices())
    ASSERT_EQ(a.fvm_data_slices(), b.fvm_data_slices());
  ASSERT_EQ(a.has_sectors_per_cluster(), b.has_sectors_per_cluster());
  if (a.has_sectors_per_cluster())
    ASSERT_EQ(a.sectors_per_cluster(), b.sectors_per_cluster());
}

TEST(MountOptionsTest, DefaultOptions) {
  MountOptions options;
  fuchsia_fs_startup::wire::StartOptions expected_start_options{
      // This is the default, but we explicitly enumerate it here to be clear that it's the default.
      .write_compression_algorithm = fuchsia_fs_startup::wire::CompressionAlgorithm::kZstdChunked,
      .write_compression_level = -1,
      .cache_eviction_policy_override = fuchsia_fs_startup::wire::EvictionPolicyOverride::kNone,
  };

  auto start_options_or = options.as_start_options();
  ASSERT_TRUE(start_options_or.is_ok()) << start_options_or.status_string();
  AssertStartOptionsEqual(*start_options_or, expected_start_options);
}

TEST(MountOptionsTest, AllOptionsSet) {
  MountOptions options{
      .readonly = true,
      .verbose_mount = true,
      .write_compression_algorithm = "UNCOMPRESSED",
      .write_compression_level = 10,
      .cache_eviction_policy = "NEVER_EVICT",
      .fsck_after_every_transaction = true,
      .allow_delivery_blobs = true,
  };
  fuchsia_fs_startup::wire::StartOptions expected_start_options{
      .read_only = true,
      .verbose = true,
      .write_compression_algorithm = fuchsia_fs_startup::wire::CompressionAlgorithm::kUncompressed,
      .write_compression_level = 10,
      .cache_eviction_policy_override =
          fuchsia_fs_startup::wire::EvictionPolicyOverride::kNeverEvict,
      .allow_delivery_blobs = true,
  };

  auto start_options_or = options.as_start_options();
  ASSERT_TRUE(start_options_or.is_ok()) << start_options_or.status_string();
  AssertStartOptionsEqual(*start_options_or, expected_start_options);
}

TEST(MountOptionsTest, ZstdChunkedEvictImmediately) {
  MountOptions options{
      .write_compression_algorithm = "ZSTD_CHUNKED",
      .cache_eviction_policy = "EVICT_IMMEDIATELY",
  };
  fuchsia_fs_startup::wire::StartOptions expected_start_options{
      .write_compression_algorithm = fuchsia_fs_startup::wire::CompressionAlgorithm::kZstdChunked,
      .write_compression_level = -1,
      .cache_eviction_policy_override =
          fuchsia_fs_startup::wire::EvictionPolicyOverride::kEvictImmediately,
  };

  auto start_options_or = options.as_start_options();
  ASSERT_TRUE(start_options_or.is_ok()) << start_options_or.status_string();
  AssertStartOptionsEqual(*start_options_or, expected_start_options);
}

TEST(MkfsOptionsTest, DefaultOptions) {
  MkfsOptions options;
  fidl::Arena arena;
  auto expected_format_options = fuchsia_fs_startup::wire::FormatOptions::Builder(arena)
                                     .verbose(false)
                                     .deprecated_padded_blobfs_format(false)
                                     .fvm_data_slices(1)
                                     .Build();

  AssertFormatOptionsEqual(options.as_format_options(arena), expected_format_options);
}

TEST(MkfsOptionsTest, AllOptionsSet) {
  MkfsOptions options{
      .fvm_data_slices = 10,
      .verbose = true,
      .sectors_per_cluster = 2,
      .deprecated_padded_blobfs_format = true,
      .num_inodes = 100,
  };
  fidl::Arena arena;
  auto expected_format_options = fuchsia_fs_startup::wire::FormatOptions::Builder(arena)
                                     .fvm_data_slices(10)
                                     .verbose(true)
                                     .deprecated_padded_blobfs_format(true)
                                     .num_inodes(100)
                                     .sectors_per_cluster(2)
                                     .Build();

  AssertFormatOptionsEqual(options.as_format_options(arena), expected_format_options);
}

}  // namespace
}  // namespace fs_management
