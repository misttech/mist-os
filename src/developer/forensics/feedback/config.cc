// Copyright 2021 The Fuchsia Authors.All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/feedback/config.h"

#include <lib/syslog/cpp/macros.h>

#include <optional>

#include "src/developer/forensics/feedback/constants.h"
#include "src/developer/forensics/utils/storage_size.h"
#include "src/lib/files/file.h"
#include "third_party/rapidjson/include/rapidjson/document.h"
#include "third_party/rapidjson/include/rapidjson/error/en.h"
#include "third_party/rapidjson/include/rapidjson/error/error.h"
#include "third_party/rapidjson/include/rapidjson/schema.h"
#include "third_party/rapidjson/include/rapidjson/stringbuffer.h"

namespace forensics::feedback {
namespace {

template <typename T>
std::optional<T> ReadConfig(const std::string& schema_str,
                            std::function<std::optional<T>(const rapidjson::Document&)> convert_fn,
                            const std::string& filepath) {
  std::string config_str;
  if (!files::ReadFileToString(filepath, &config_str)) {
    FX_LOGS(ERROR) << "Error reading config file at " << filepath;
    return std::nullopt;
  }

  rapidjson::Document config;
  if (const rapidjson::ParseResult result = config.Parse(config_str.c_str()); !result) {
    FX_LOGS(ERROR) << "Error parsing config as JSON at offset " << result.Offset() << " "
                   << rapidjson::GetParseError_En(result.Code());
    return std::nullopt;
  }

  rapidjson::Document schema;
  if (const rapidjson::ParseResult result = schema.Parse(schema_str); !result) {
    FX_LOGS(ERROR) << "Error parsing config schema at offset " << result.Offset() << " "
                   << rapidjson::GetParseError_En(result.Code());
    return std::nullopt;
  }

  rapidjson::SchemaDocument schema_doc(schema);
  if (rapidjson::SchemaValidator validator(schema_doc); !config.Accept(validator)) {
    rapidjson::StringBuffer buf;
    validator.GetInvalidSchemaPointer().StringifyUriFragment(buf);
    FX_LOGS(ERROR) << "Config does not match schema, violating '"
                   << validator.GetInvalidSchemaKeyword() << "' rule";
    return std::nullopt;
  }

  return convert_fn(config);
}

template <typename T>
std::optional<T> GetConfig(const std::string& schema_str,
                           std::function<std::optional<T>(const rapidjson::Document&)> convert_fn,
                           const std::string& config_type, const std::string& default_path,
                           const std::optional<std::string>& override_path) {
  std::optional<T> config;
  if (override_path.has_value() && files::IsFile(*override_path)) {
    if (config = ReadConfig<T>(schema_str, convert_fn, *override_path); !config.has_value()) {
      FX_LOGS(ERROR) << "Failed to read override " << config_type << " config file at "
                     << *override_path;
    }
  }

  if (!config.has_value()) {
    if (config = ReadConfig<T>(schema_str, convert_fn, default_path); !config.has_value()) {
      FX_LOGS(ERROR) << "Failed to read default " << config_type << " config file at "
                     << default_path;
    }
  }

  return config;
}

constexpr char kProductConfigSchema[] = R"({
    "type": "object",
    "properties": {
       "persisted_logs_num_files": {
           "type": "number"
       },
       "persisted_logs_total_size_kib": {
           "type": "number"
       },
       "snapshot_persistence_max_tmp_size_mib": {
           "type": "number"
       },
       "snapshot_persistence_max_cache_size_mib": {
           "type": "number"
       }
    },
    "required": [
       "persisted_logs_num_files",
       "persisted_logs_total_size_kib",
       "snapshot_persistence_max_tmp_size_mib",
       "snapshot_persistence_max_cache_size_mib"
    ],
    "additionalProperties": false
})";

std::optional<ProductConfig> ParseProductConfig(const rapidjson::Document& json) {
  ProductConfig config;
  if (const int64_t num_files = json[kPersistedLogsNumFilesKey].GetInt64(); num_files > 0) {
    config.persisted_logs_num_files = num_files;
  } else {
    config.persisted_logs_num_files = std::nullopt;
  }

  if (const int64_t total_size_kib = json[kPersistedLogsTotalSizeKey].GetInt64();
      total_size_kib > 0) {
    config.persisted_logs_total_size = StorageSize::Kilobytes(total_size_kib);
  } else {
    config.persisted_logs_total_size = std::nullopt;
  }

  if (config.persisted_logs_num_files.has_value() ^ config.persisted_logs_total_size.has_value()) {
    FX_LOGS(ERROR)
        << "Can't only have one of the two persisted_logs fields set to a positive value";
    return std::nullopt;
  }

  if (const int64_t max_tmp_size_mib = json[kSnapshotPersistenceMaxTmpSizeKey].GetInt64();
      max_tmp_size_mib > 0) {
    config.snapshot_persistence_max_tmp_size = StorageSize::Megabytes(max_tmp_size_mib);
  } else {
    config.snapshot_persistence_max_tmp_size = std::nullopt;
  }

  if (const int64_t max_cache_size_mib = json[kSnapshotPersistenceMaxCacheSizeKey].GetInt64();
      max_cache_size_mib > 0) {
    config.snapshot_persistence_max_cache_size = StorageSize::Megabytes(max_cache_size_mib);
  } else {
    config.snapshot_persistence_max_cache_size = std::nullopt;
  }

  return config;
}

const char kBuildTypeConfigSchema[] = R"({
  "type": "object",
  "properties": {
    "crash_report_upload_policy": {
      "type": "string",
      "enum": [
        "disabled",
        "enabled",
        "read_from_privacy_settings"
      ]
    },
    "daily_per_product_crash_report_quota": {
      "type": "number"
    },
    "enable_data_redaction": {
      "type": "boolean"
    },
    "enable_hourly_snapshots": {
      "type": "boolean"
    },
    "enable_limit_inspect_data": {
      "type": "boolean"
    }
  },
  "required": [
    "crash_report_upload_policy",
    "daily_per_product_crash_report_quota",
    "enable_data_redaction",
    "enable_hourly_snapshots",
    "enable_limit_inspect_data"
  ],
  "additionalProperties": false
})";

std::optional<BuildTypeConfig> ParseBuildTypeConfig(const rapidjson::Document& json) {
  BuildTypeConfig config{
      .enable_data_redaction = json[kEnableDataRedactionKey].GetBool(),
      .enable_hourly_snapshots = json[kEnableHourlySnapshotsKey].GetBool(),
      .enable_limit_inspect_data = json[kEnableLimitInspectDataKey].GetBool(),
  };

  if (const std::string policy = json[kCrashReportUploadPolicyKey].GetString();
      policy == "disabled") {
    config.crash_report_upload_policy = CrashReportUploadPolicy::kDisabled;
  } else if (policy == "enabled") {
    config.crash_report_upload_policy = CrashReportUploadPolicy::kEnabled;
  } else if (policy == "read_from_privacy_settings") {
    config.crash_report_upload_policy = CrashReportUploadPolicy::kReadFromPrivacySettings;
  } else {
    FX_LOGS(FATAL) << "Upload policy '" << policy << "' not permitted by schema";
  }

  if (const int64_t quota = json[kDailyPerProductCrashReportQuotaKey].GetInt64(); quota > 0) {
    config.daily_per_product_crash_report_quota = quota;
  } else {
    config.daily_per_product_crash_report_quota = std::nullopt;
  }

  return config;
}

constexpr char kSnapshotConfigSchema[] = R"({
  "type": "object",
  "properties": {
    "annotation_allowlist": {
      "type": "array",
      "items": {
        "type": "string"
      },
      "uniqueItems": true
    },
    "attachment_allowlist": {
      "type": "array",
      "items": {
        "type": "string"
      },
      "uniqueItems": true
    }
  },
  "required": [
    "annotation_allowlist",
    "attachment_allowlist"
  ],
  "additionalProperties": false
})";

std::optional<SnapshotConfig> ParseSnapshotConfig(const rapidjson::Document& json) {
  SnapshotConfig config;
  for (const auto& k : json["annotation_allowlist"].GetArray()) {
    config.annotation_allowlist.insert(k.GetString());
  }

  for (const auto& k : json["attachment_allowlist"].GetArray()) {
    config.attachment_allowlist.insert(k.GetString());
  }

  return config;
}

}  // namespace

std::optional<ProductConfig> GetProductConfig(const std::string& default_path,
                                              const std::string& override_path) {
  return GetConfig<ProductConfig>(kProductConfigSchema, ParseProductConfig, "product", default_path,
                                  override_path);
}

std::optional<BuildTypeConfig> GetBuildTypeConfig(const std::string& default_path,
                                                  const std::string& override_path) {
  return GetConfig<BuildTypeConfig>(kBuildTypeConfigSchema, ParseBuildTypeConfig, "build type",
                                    default_path, override_path);
}

std::optional<SnapshotConfig> GetSnapshotConfig(const std::string& default_path) {
  return GetConfig<SnapshotConfig>(kSnapshotConfigSchema, ParseSnapshotConfig, "snapshot",
                                   default_path, std::nullopt);
}

void ExposeConfig(inspect::Node& inspect_root, const BuildTypeConfig& build_type_config,
                  const ProductConfig& product_config) {
  const std::string crash_report_quota =
      build_type_config.daily_per_product_crash_report_quota.has_value()
          ? std::to_string(*build_type_config.daily_per_product_crash_report_quota)
          : "none";

  const std::string persisted_logs_num_files =
      product_config.persisted_logs_num_files.has_value()
          ? std::to_string(*product_config.persisted_logs_num_files)
          : "none";

  const std::string persisted_logs_total_size =
      product_config.persisted_logs_total_size.has_value()
          ? std::to_string(product_config.persisted_logs_total_size->ToKilobytes())
          : "none";

  const std::string snapshot_persistence_tmp_size =
      product_config.snapshot_persistence_max_tmp_size.has_value()
          ? std::to_string(product_config.snapshot_persistence_max_tmp_size->ToMegabytes())
          : "none";

  const std::string snapshot_persistence_cache_size =
      product_config.snapshot_persistence_max_cache_size.has_value()
          ? std::to_string(product_config.snapshot_persistence_max_cache_size->ToMegabytes())
          : "none";

  inspect_root.RecordChild(
      kInspectConfigKey, [&build_type_config, &crash_report_quota, &persisted_logs_num_files,
                          &persisted_logs_total_size, &snapshot_persistence_tmp_size,
                          &snapshot_persistence_cache_size](inspect::Node& node) {
        node.RecordString(kCrashReportUploadPolicyKey,
                          ToString(build_type_config.crash_report_upload_policy));
        node.RecordString(kDailyPerProductCrashReportQuotaKey, crash_report_quota);
        node.RecordBool(kEnableDataRedactionKey, build_type_config.enable_data_redaction);
        node.RecordBool(kEnableHourlySnapshotsKey, build_type_config.enable_hourly_snapshots);
        node.RecordBool(kEnableLimitInspectDataKey, build_type_config.enable_limit_inspect_data);

        node.RecordString(kPersistedLogsNumFilesKey, persisted_logs_num_files);
        node.RecordString(kPersistedLogsTotalSizeKey, persisted_logs_total_size);
        node.RecordString(kSnapshotPersistenceMaxTmpSizeKey, snapshot_persistence_tmp_size);
        node.RecordString(kSnapshotPersistenceMaxCacheSizeKey, snapshot_persistence_cache_size);
      });
}

std::string ToString(const CrashReportUploadPolicy upload_policy) {
  switch (upload_policy) {
    case CrashReportUploadPolicy::kDisabled:
      return "DISABLED";
    case CrashReportUploadPolicy::kEnabled:
      return "ENABLED";
    case CrashReportUploadPolicy::kReadFromPrivacySettings:
      return "READ_FROM_PRIVACY_SETTINGS";
  }
}

}  // namespace forensics::feedback
