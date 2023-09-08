// Copyright 2021 The Fuchsia Authors.All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FORENSICS_FEEDBACK_CONFIG_H_
#define SRC_DEVELOPER_FORENSICS_FEEDBACK_CONFIG_H_

#include <lib/inspect/cpp/vmo/types.h>

#include <optional>
#include <set>
#include <string>

#include "src/developer/forensics/feedback/constants.h"
#include "src/developer/forensics/utils/storage_size.h"

namespace forensics::feedback {

// Policy defining whether to upload pending and future crash reports to a remote crash server.
enum class CrashReportUploadPolicy {
  // Crash reports should not be uploaded and be kept in the store.
  kDisabled,

  // Crash reports should be uploaded and on success removed from the store, if present.
  // If the upload is unsuccessful and the policy changes to kDisabled, the crash report should
  // follow the kDisabled policy.
  kEnabled,

  // Policy should not be read from the config, but instead from the privacy settings.
  kReadFromPrivacySettings,
};

struct ProductConfig {
  std::optional<uint64_t> persisted_logs_num_files;
  std::optional<StorageSize> persisted_logs_total_size;
  std::optional<StorageSize> snapshot_persistence_max_tmp_size;
  std::optional<StorageSize> snapshot_persistence_max_cache_size;
};

struct BuildTypeConfig {
  CrashReportUploadPolicy crash_report_upload_policy;
  std::optional<uint64_t> daily_per_product_crash_report_quota;
  bool enable_data_redaction;
  bool enable_hourly_snapshots;
  bool enable_limit_inspect_data;
};

struct SnapshotConfig {
  std::set<std::string> annotation_allowlist;
  std::set<std::string> attachment_allowlist;
};

std::optional<ProductConfig> GetProductConfig(
    const std::string& default_path = kDefaultProductConfigPath,
    const std::string& override_path = kOverrideProductConfigPath);

std::optional<BuildTypeConfig> GetBuildTypeConfig(
    const std::string& default_path = kDefaultBuildTypeConfigPath,
    const std::string& override_path = kOverrideBuildTypeConfigPath);

std::optional<SnapshotConfig> GetSnapshotConfig(
    const std::string& path = kDefaultSnapshotConfigPath);

// Exposes the static configuration based on build type and product.
void ExposeConfig(inspect::Node& inspect_root, const BuildTypeConfig& build_type_config,
                  const ProductConfig& product_config);

// Returns the string version of the enum.
std::string ToString(CrashReportUploadPolicy upload_policy);

}  // namespace forensics::feedback

#endif  // SRC_DEVELOPER_FORENSICS_FEEDBACK_CONFIG_H_
