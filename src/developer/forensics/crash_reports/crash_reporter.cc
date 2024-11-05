// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/crash_reports/crash_reporter.h"

#include <fuchsia/mem/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/fit/defer.h>
#include <lib/fpromise/promise.h>
#include <lib/fpromise/result.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/clock.h>
#include <lib/zx/time.h>
#include <zircon/errors.h>
#include <zircon/types.h>
#include <zircon/utc.h>

#include <map>
#include <memory>
#include <optional>
#include <string>
#include <tuple>
#include <utility>
#include <variant>

#include "src/developer/forensics/crash_reports/constants.h"
#include "src/developer/forensics/crash_reports/crash_server.h"
#include "src/developer/forensics/crash_reports/filing_result.h"
#include "src/developer/forensics/crash_reports/product.h"
#include "src/developer/forensics/crash_reports/report.h"
#include "src/developer/forensics/crash_reports/report_util.h"
#include "src/developer/forensics/feedback/config.h"
#include "src/developer/forensics/feedback/constants.h"
#include "src/developer/forensics/utils/cobalt/metrics.h"

namespace forensics {
namespace crash_reports {
namespace {

using FidlSnapshot = fuchsia::feedback::Snapshot;
using fuchsia::feedback::CrashReport;
using fuchsia::feedback::CrashReporter_FileReport_Result;
using fuchsia::feedback::FileReportResults;
using fuchsia::feedback::FilingError;
using fuchsia::feedback::FilingSuccess;

constexpr zx::duration kSnapshotTimeout = zx::min(1);

// Returns what the initial ReportId should be, based on the contents of the report store in the
// filesystem.
//
// Note: This function traverses report store in the filesystem and should be used sparingly.
ReportId SeedReportId() {
  // The next ReportId will be one more than the largest in the report store.
  auto all_report_ids = ReportStoreMetadata(kReportStoreTmpPath, kReportStoreMaxTmpSize).Reports();
  const auto all_cache_report_ids =
      ReportStoreMetadata(kReportStoreCachePath, kReportStoreMaxCacheSize).Reports();
  all_report_ids.insert(all_report_ids.end(), all_cache_report_ids.begin(),
                        all_cache_report_ids.end());

  std::sort(all_report_ids.begin(), all_report_ids.end());
  return (all_report_ids.empty()) ? 0u : all_report_ids.back() + 1;
}

// Make the appropriate ReportingPolicyWatcher for the upload policy in |config|.
std::unique_ptr<ReportingPolicyWatcher> MakeReportingPolicyWatcher(
    async_dispatcher_t* dispatcher, std::shared_ptr<sys::ServiceDirectory> services,
    const feedback::CrashReportUploadPolicy policy) {
  switch (policy) {
    case feedback::CrashReportUploadPolicy::kEnabled:
      // Uploads being enabled in |config| is explicit consent to upload all reports.
      return std::make_unique<StaticReportingPolicyWatcher<ReportingPolicy::kUpload>>();
    case feedback::CrashReportUploadPolicy::kDisabled:
      // Uploads being disabled in |config| means that reports should be archived.
      return std::make_unique<StaticReportingPolicyWatcher<ReportingPolicy::kArchive>>();
    case feedback::CrashReportUploadPolicy::kReadFromPrivacySettings:
      return std::make_unique<UserReportingPolicyWatcher>(dispatcher, std::move(services));
  }
}

CrashReporter_FileReport_Result InternalResultsToFidl(const FilingResult result,
                                                      const std::string& report_id = "") {
  const fpromise::result<FilingSuccess, FilingError> fidl_result = ToFidlFilingResult(result);
  if (fidl_result.is_error()) {
    return fpromise::error(fidl_result.error());
  }

  FileReportResults results;
  results.set_result(ToFidlFilingResult(result).value());
  results.set_report_id(report_id);

  return fpromise::ok(std::move(results));
}

}  // namespace

CrashReporter::CrashReporter(
    async_dispatcher_t* dispatcher, const std::shared_ptr<sys::ServiceDirectory>& services,
    timekeeper::Clock* clock, zx::unowned_clock clock_handle,
    const std::shared_ptr<InfoContext>& info_context, feedback::BuildTypeConfig build_type_config,
    CrashRegister* crash_register, LogTags* tags, CrashServer* crash_server,
    ReportStore* report_store, feedback_data::DataProviderInternal* data_provider,
    zx::duration snapshot_collector_window_duration, const zx::duration product_quota_reset_offset)
    : dispatcher_(dispatcher),
      executor_(dispatcher),
      services_(services),
      clock_(clock),
      tags_(tags),
      crash_register_(crash_register),
      utc_clock_ready_watcher_(dispatcher_, std::move(clock_handle)),
      utc_provider_(&utc_clock_ready_watcher_, clock_),
      crash_server_(crash_server),
      snapshot_store_(report_store->GetSnapshotStore()),
      queue_(dispatcher_, services_, info_context, tags_, report_store, crash_server_),
      snapshot_collector_(dispatcher, clock_, data_provider, report_store->GetSnapshotStore(),
                          &queue_, snapshot_collector_window_duration),
      product_quotas_(clock_, build_type_config.daily_per_product_crash_report_quota,
                      feedback::kProductQuotasPath, &utc_clock_ready_watcher_,
                      product_quota_reset_offset),
      info_(info_context),
      reporting_policy_watcher_(MakeReportingPolicyWatcher(
          dispatcher_, services, build_type_config.crash_report_upload_policy)) {
  FX_CHECK(dispatcher_);
  FX_CHECK(services_);
  FX_CHECK(crash_register_);
  FX_CHECK(crash_server_);

  // If crash reports won't be uploaded, there shouldn't be a quota in the config.
  if (build_type_config.crash_report_upload_policy ==
      feedback::CrashReportUploadPolicy::kDisabled) {
    const auto quota = build_type_config.daily_per_product_crash_report_quota;
    FX_CHECK(!quota.has_value()) << "Can't have quota when upload policy is disabled: quota is "
                                 << *quota;
  }

  next_report_id_ = SeedReportId();

  queue_.WatchReportingPolicy(reporting_policy_watcher_.get());
  info_.ExposeReportingPolicy(reporting_policy_watcher_.get());

  if (build_type_config.enable_hourly_snapshots) {
    // We schedule the first hourly snapshot in 5 minutes and then it will auto-schedule itself
    // every hour after that.
    ScheduleHourlySnapshot(zx::min(5));
  }
}

void CrashReporter::SetNetworkIsReachable(const bool is_reachable) {
  queue_.SetNetworkIsReachable(is_reachable);
}

void CrashReporter::PersistAllCrashReports() {
  queue_.StopUploading();
  snapshot_collector_.Shutdown();
}

void CrashReporter::FileReport(fuchsia::feedback::CrashReport report, FileReportCallback callback) {
  if (!report.has_program_name()) {
    FX_LOGS(ERROR) << "Input report missing required program name. Won't file.";
    callback(InternalResultsToFidl(FilingResult::kInvalidArgsError));
    info_.LogCrashState(cobalt::CrashState::kDropped);
    return;
  }

  File(std::move(report), /*is_hourly_snapshot=*/false, std::move(callback));
}

void CrashReporter::File(fuchsia::feedback::CrashReport report, const bool is_hourly_snapshot,
                         FileReportCallback callback) {
  if (reporting_policy_watcher_->CurrentPolicy() == ReportingPolicy::kDoNotFileAndDelete) {
    callback(InternalResultsToFidl(FilingResult::kReportNotFiledUserOptedOut));
    info_.LogCrashState(cobalt::CrashState::kDeleted);
    return;
  }

  const auto program_name = report.program_name();
  const auto report_id = next_report_id_++;

  // Fetch the product as close to the crash as possible. The product may be re-registered / changed
  // after the crash and getting it now is an attempt to mitigate that race.
  const auto product = crash_register_->HasProduct(program_name)
                           ? crash_register_->GetProduct(program_name)
                           : Product::DefaultPlatformProduct();

  tags_->Register(report_id, {Logname(program_name)});

  if (!product_quotas_.HasQuotaRemaining(product)) {
    FX_LOGST(INFO, tags_->Get(report_id)) << "Daily report quota reached. Won't retry";
    callback(InternalResultsToFidl(FilingResult::kQuotaReachedError));
    info_.LogCrashState(cobalt::CrashState::kOnDeviceQuotaReached);
    tags_->Unregister(report_id);
    return;
  }

  product_quotas_.DecrementRemainingQuota(product);

  if (is_hourly_snapshot) {
    FX_LOGST(INFO, tags_->Get(report_id)) << "Generating hourly snapshot";
  } else {
    FX_LOGST(INFO, tags_->Get(report_id)) << "Generating report";
  }

  const auto current_time = utc_provider_.CurrentTime();

  auto p = snapshot_collector_
               .GetReport(kSnapshotTimeout, std::move(report), report_id, current_time, product,
                          is_hourly_snapshot, reporting_policy_watcher_->CurrentPolicy())
               .then([this, report_id, is_hourly_snapshot,
                      callback = std::move(callback)](fpromise::result<Report>& result) mutable {
                 if (is_hourly_snapshot) {
                   FX_LOGST(INFO, tags_->Get(report_id)) << "Generated hourly snapshot";
                 } else {
                   FX_LOGST(INFO, tags_->Get(report_id)) << "Generated report";
                 }

                 // Logs a cobalt event and error message on why filing |report| didn't succeed.
                 auto record_failure = [this, report_id](const auto cobalt_error, const auto log) {
                   FX_LOGST(ERROR, tags_->Get(report_id)) << log;
                   info_.LogCrashState(cobalt_error);
                   tags_->Unregister(report_id);
                 };

                 if (!result.is_ok()) {
                   return record_failure(cobalt::CrashState::kDropped,
                                         "Failed to file report: MakeReport failed. Won't retry");
                 }

                 if (!queue_.Add(std::move(result.value()),
                                 [callback = std::move(callback)](const FilingResult& result,
                                                                  const std::string& report_id) {
                                   callback(InternalResultsToFidl(result, report_id));
                                 })) {
                   return record_failure(cobalt::CrashState::kDropped,
                                         "Failed to file report: Queue::Add failed. Won't retry");
                 }

                 info_.LogCrashState(cobalt::CrashState::kFiled);
               });

  executor_.schedule_task(std::move(p));
}

void CrashReporter::ScheduleHourlySnapshot(const zx::duration delay) {
  async::PostDelayedTask(
      dispatcher_,
      [this]() {
        auto schedule_next = ::fit::defer([this] { ScheduleHourlySnapshot(zx::hour(1)); });

        if (queue_.HasHourlyReport()) {
          FX_LOGS(INFO) << "Skipping hourly snapshot as the last one has not been uploaded yet "
                           "– connectivity issues?";
          return;
        }

        fuchsia::feedback::CrashReport report;
        report.set_program_name(kHourlySnapshotProgramName)
            .set_program_uptime(clock_->MonotonicNow().get())
            .set_is_fatal(false)
            .set_crash_signature(kHourlySnapshotSignature);

        File(std::move(report), /*is_hourly_snapshot=*/true,
             [](const CrashReporter_FileReport_Result& result) {});
      },
      delay);
}

}  // namespace crash_reports
}  // namespace forensics
