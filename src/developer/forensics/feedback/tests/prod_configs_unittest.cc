// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <optional>
#include <string>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/developer/forensics/feedback/config.h"
#include "src/developer/forensics/utils/storage_size.h"
#include "src/lib/files/path.h"

namespace forensics::feedback {
namespace {

using testing::UnorderedElementsAreArray;

class ProdConfigTest : public testing::Test {
 public:
  static std::optional<ProductConfig> ReadProductConfig(const std::string& config_filename) {
    return GetProductConfig(files::JoinPath("/pkg/data/product/configs", config_filename));
  }

  static std::optional<BuildTypeConfig> ReadBuildTypeConfig(const std::string& config_filename) {
    return GetBuildTypeConfig(files::JoinPath("/pkg/data/build_type/configs", config_filename));
  }

  static std::optional<SnapshotConfig> ReadSnapshotConfig(const std::string& config_filename) {
    return GetSnapshotConfig(files::JoinPath("/pkg/data/snapshot/configs", config_filename));
  }
};

TEST_F(ProdConfigTest, DefaultProduct) {
  const std::optional<ProductConfig> config = ReadProductConfig("default.json");
  ASSERT_TRUE(config.has_value());

  EXPECT_EQ(config->persisted_logs_num_files, 8u);
  EXPECT_EQ(config->persisted_logs_total_size, StorageSize::Kilobytes(512));
  EXPECT_FALSE(config->snapshot_persistence_max_tmp_size.has_value());
  EXPECT_FALSE(config->snapshot_persistence_max_cache_size.has_value());
}

TEST_F(ProdConfigTest, LargeDiskProduct) {
  const std::optional<ProductConfig> config = ReadProductConfig("large_disk.json");
  ASSERT_TRUE(config.has_value());

  EXPECT_EQ(config->persisted_logs_num_files, 8u);
  EXPECT_EQ(config->persisted_logs_total_size, StorageSize::Kilobytes(512));
  EXPECT_EQ(config->snapshot_persistence_max_tmp_size, StorageSize::Megabytes(10));
  EXPECT_EQ(config->snapshot_persistence_max_cache_size, StorageSize::Megabytes(10));
}

TEST_F(ProdConfigTest, DefaultBuildType) {
  const std::optional<BuildTypeConfig> config = ReadBuildTypeConfig("default.json");
  ASSERT_TRUE(config.has_value());

  EXPECT_EQ(config->crash_report_upload_policy, CrashReportUploadPolicy::kDisabled);
  EXPECT_EQ(config->daily_per_product_crash_report_quota, std::nullopt);
  EXPECT_FALSE(config->enable_data_redaction);
  EXPECT_FALSE(config->enable_hourly_snapshots);
  EXPECT_FALSE(config->enable_limit_inspect_data);
}

TEST_F(ProdConfigTest, User) {
  const std::optional<BuildTypeConfig> config = ReadBuildTypeConfig("user.json");
  ASSERT_TRUE(config.has_value());

  EXPECT_EQ(config->crash_report_upload_policy, CrashReportUploadPolicy::kReadFromPrivacySettings);
  EXPECT_EQ(config->daily_per_product_crash_report_quota, 100);
  EXPECT_TRUE(config->enable_data_redaction);
  EXPECT_FALSE(config->enable_hourly_snapshots);
  EXPECT_TRUE(config->enable_limit_inspect_data);
}

TEST_F(ProdConfigTest, Userdebug) {
  const std::optional<BuildTypeConfig> config = ReadBuildTypeConfig("userdebug.json");
  ASSERT_TRUE(config.has_value());

  EXPECT_EQ(config->crash_report_upload_policy, CrashReportUploadPolicy::kReadFromPrivacySettings);
  EXPECT_EQ(config->daily_per_product_crash_report_quota, std::nullopt);
  EXPECT_FALSE(config->enable_data_redaction);
  EXPECT_TRUE(config->enable_hourly_snapshots);
  EXPECT_FALSE(config->enable_limit_inspect_data);
}

TEST_F(ProdConfigTest, DefaultSnapshot) {
  const std::optional<SnapshotConfig> config = ReadSnapshotConfig("default.json");
  ASSERT_TRUE(config.has_value());

  EXPECT_THAT(config->annotation_allowlist, UnorderedElementsAreArray({
                                                "build.board",
                                                "build.is_debug",
                                                "build.latest-commit-date",
                                                "build.product",
                                                "build.version",
                                                "build.version.previous-boot",
                                                "build.platform.version",
                                                "build.platform.version.previous-boot",
                                                "build.product.version",
                                                "build.product.version.previous-boot",
                                                "device.board-name",
                                                "device.feedback-id",
                                                "device.num-cpus",
                                                "device.runtime",
                                                "device.uptime",
                                                "device.utc-time",
                                                "hardware.board.name",
                                                "hardware.board.revision",
                                                "hardware.product.language",
                                                "hardware.product.locale-list",
                                                "hardware.product.manufacturer",
                                                "hardware.product.model",
                                                "hardware.product.name",
                                                "hardware.product.regulatory-domain",
                                                "hardware.product.sku",
                                                "system.boot-id.current",
                                                "system.boot-id.previous",
                                                "system.last-reboot.reason",
                                                "system.last-reboot.uptime",
                                                "system.locale.primary",
                                                "system.timezone.primary",
                                                "system.update-channel.current",
                                                "system.update-channel.target",
                                                "system.user-activity.current.state",
                                                "system.user-activity.current.duration",
                                            }));

  EXPECT_THAT(config->attachment_allowlist, UnorderedElementsAreArray({
                                                "build.snapshot.xml",
                                                "build.kernel-boot-options.txt",
                                                "inspect.json",
                                                "log.kernel.txt",
                                                "log.system.previous_boot.txt",
                                                "log.system.txt",
                                            }));
}

}  // namespace
}  // namespace forensics::feedback
