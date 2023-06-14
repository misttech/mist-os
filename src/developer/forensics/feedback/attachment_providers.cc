// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/feedback/attachment_providers.h"

#include "src/developer/forensics/feedback_data/constants.h"

namespace forensics::feedback {

AttachmentProviders::AttachmentProviders(async_dispatcher_t* dispatcher,
                                         std::shared_ptr<sys::ServiceDirectory> services,
                                         std::optional<zx::duration> delete_previous_boot_log_at,
                                         timekeeper::Clock* clock, RedactorBase* redactor,
                                         feedback_data::InspectDataBudget* inspect_data_budget,
                                         std::set<std::string> allowlist)
    : kernel_log_(dispatcher, services, AttachmentProviderBackoff(), redactor),
      system_log_(dispatcher, services, clock, redactor, feedback_data::kActiveLoggingPeriod),
      inspect_(dispatcher, services, AttachmentProviderBackoff(), inspect_data_budget, redactor),
      previous_boot_log_(dispatcher, clock, delete_previous_boot_log_at,
                         feedback_data::kPreviousLogsFilePath),
      build_snapshot_(feedback_data::kBuildSnapshotPath),
      attachment_manager_(dispatcher, allowlist,
                          {
                              {feedback_data::kAttachmentLogKernel, &kernel_log_},
                              {feedback_data::kAttachmentLogSystem, &system_log_},
                              {feedback_data::kAttachmentLogSystemPrevious, &previous_boot_log_},
                              {feedback_data::kAttachmentInspect, &inspect_},
                              {feedback_data::kAttachmentBuildSnapshot, &build_snapshot_},
                          }) {
  if (allowlist.empty()) {
    FX_LOGS(WARNING)
        << "Attachment allowlist is empty, no platform attachments will be collected or returned";
  }
}

std::unique_ptr<backoff::Backoff> AttachmentProviders::AttachmentProviderBackoff() {
  return std::unique_ptr<backoff::Backoff>(
      new backoff::ExponentialBackoff(zx::min(1), 2u, zx::hour(1)));
}

}  // namespace forensics::feedback
