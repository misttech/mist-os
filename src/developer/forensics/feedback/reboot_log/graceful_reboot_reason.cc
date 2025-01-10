// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/feedback/reboot_log/graceful_reboot_reason.h"

#include <fcntl.h>
#include <lib/syslog/cpp/macros.h>

#include <string>

#include <fbl/unique_fd.h>

#include "src/lib/files/file_descriptor.h"
#include "src/lib/fxl/strings/join_strings.h"
#include "src/lib/fxl/strings/split_string.h"

namespace forensics {
namespace feedback {
namespace {

constexpr char kReasonNotSet[] = "NOT SET";
constexpr char kReasonUserRequest[] = "USER REQUEST";
constexpr char kReasonSystemUpdate[] = "SYSTEM UPDATE";
constexpr char kReasonRetrySystemUpdate[] = "RETRY SYSTEM UPDATE";
constexpr char kReasonHighTemperature[] = "HIGH TEMPERATURE";
constexpr char kReasonSessionFailure[] = "SESSION FAILURE";
constexpr char kReasonSysmgrFailure[] = "SYSMGR FAILURE";
constexpr char kReasonCriticalComponentFailure[] = "CRITICAL COMPONENT FAILURE";
constexpr char kReasonFdr[] = "FACTORY DATA RESET";
constexpr char kReasonZbiSwap[] = "ZBI SWAP";
constexpr char kOutOfMemory[] = "OUT OF MEMORY";
constexpr char kReasonNetstackMigration[] = "NETSTACK MIGRATION";
constexpr char kReasonNotSupported[] = "NOT SUPPORTED";
constexpr char kReasonNotParseable[] = "NOT PARSEABLE";

// Used to separate multiple `GracefulRebootReason` when written to file.
constexpr char kDeliminator[] = ",";

// Used to represent the absence of a `GracefulRebootReason` when written to
// file. Unlike the other "reason" strings above, this is translated to an
// empty vector rather than a `GracefulRebootReason`, when read from file.
constexpr char kNoReasons[] = "NONE";

}  // namespace

std::string ToString(const GracefulRebootReason reason) {
  switch (reason) {
    case GracefulRebootReason::kNotSet:
      return kReasonNotSet;
    case GracefulRebootReason::kUserRequest:
      return kReasonUserRequest;
    case GracefulRebootReason::kSystemUpdate:
      return kReasonSystemUpdate;
    case GracefulRebootReason::kRetrySystemUpdate:
      return kReasonRetrySystemUpdate;
    case GracefulRebootReason::kHighTemperature:
      return kReasonHighTemperature;
    case GracefulRebootReason::kSessionFailure:
      return kReasonSessionFailure;
    case GracefulRebootReason::kSysmgrFailure:
      return kReasonSysmgrFailure;
    case GracefulRebootReason::kCriticalComponentFailure:
      return kReasonCriticalComponentFailure;
    case GracefulRebootReason::kFdr:
      return kReasonFdr;
    case GracefulRebootReason::kZbiSwap:
      return kReasonZbiSwap;
    case GracefulRebootReason::kOutOfMemory:
      return kOutOfMemory;
    case GracefulRebootReason::kNetstackMigration:
      return kReasonNetstackMigration;
    case GracefulRebootReason::kNotSupported:
      return kReasonNotSupported;
    case GracefulRebootReason::kNotParseable:
      return kReasonNotParseable;
  }

  return kReasonNotSet;
}

GracefulRebootReason FromString(const std::string_view reason) {
  if (reason == kReasonUserRequest) {
    return GracefulRebootReason::kUserRequest;
  } else if (reason == kReasonSystemUpdate) {
    return GracefulRebootReason::kSystemUpdate;
  } else if (reason == kReasonRetrySystemUpdate) {
    return GracefulRebootReason::kRetrySystemUpdate;
  } else if (reason == kReasonHighTemperature) {
    return GracefulRebootReason::kHighTemperature;
  } else if (reason == kReasonSessionFailure) {
    return GracefulRebootReason::kSessionFailure;
  } else if (reason == kReasonSysmgrFailure) {
    return GracefulRebootReason::kSysmgrFailure;
  } else if (reason == kReasonCriticalComponentFailure) {
    return GracefulRebootReason::kCriticalComponentFailure;
  } else if (reason == kReasonFdr) {
    return GracefulRebootReason::kFdr;
  } else if (reason == kReasonZbiSwap) {
    return GracefulRebootReason::kZbiSwap;
  } else if (reason == kReasonNetstackMigration) {
    return GracefulRebootReason::kNetstackMigration;
  } else if (reason == kReasonNotSupported) {
    return GracefulRebootReason::kNotSupported;
  } else if (reason == kOutOfMemory) {
    return GracefulRebootReason::kOutOfMemory;
  }

  FX_LOGS(ERROR) << "Invalid persisted graceful reboot reason: " << reason;
  return GracefulRebootReason::kNotParseable;
}

// Converts the list of `GracefulRebootReasons` into a single string.
//
// The format is:
// "Reason 1,Reason 2,Reason 3"
//
// Note that some variants that should not be persisted (e.g. `kNotParseable`)
// are translated to `kNotSupported`.
std::string ToFileContent(const std::vector<GracefulRebootReason>& reasons) {
  if (reasons.empty()) {
    return kNoReasons;
  }
  std::vector<std::string> reason_strings;
  reason_strings.reserve(reasons.size());
  for (const auto& reason : reasons) {
    std::string reason_string;
    switch (reason) {
      case GracefulRebootReason::kUserRequest:
      case GracefulRebootReason::kSystemUpdate:
      case GracefulRebootReason::kRetrySystemUpdate:
      case GracefulRebootReason::kHighTemperature:
      case GracefulRebootReason::kSessionFailure:
      case GracefulRebootReason::kSysmgrFailure:
      case GracefulRebootReason::kCriticalComponentFailure:
      case GracefulRebootReason::kFdr:
      case GracefulRebootReason::kZbiSwap:
      case GracefulRebootReason::kOutOfMemory:
      case GracefulRebootReason::kNetstackMigration:
      case GracefulRebootReason::kNotSupported:
        reason_string = ToString(reason);
        break;
      case GracefulRebootReason::kNotSet:
      case GracefulRebootReason::kNotParseable:
        FX_LOGS(ERROR) << "Invalid persisted graceful reboot reason: " << ToString(reason);
        reason_string = kReasonNotSupported;
        break;
    }
    if (reason_string.empty()) {
      // The reason was out of the valid bounds of a `GracefulRebootReason`
      // (None of the switch cases above applied).
      reason_string = kReasonNotSupported;
    }

    reason_strings.push_back(reason_string);
  }
  return fxl::JoinStrings(reason_strings, kDeliminator);
}

// Like `ToFileContent`, but does not perform any translation of the reasons.
std::string ToLog(const std::vector<GracefulRebootReason>& reasons) {
  if (reasons.empty()) {
    return kNoReasons;
  }
  std::vector<std::string> reason_strings;
  reason_strings.reserve(reasons.size());
  for (const auto& reason : reasons) {
    reason_strings.push_back(ToString(reason));
  }
  return fxl::JoinStrings(reason_strings, kDeliminator);
}

// Converts the file contents into a list of `GracefulRebootReason`.
//
// The expected format is:
// "Reason 1,Reason 2,Reason 3"
//
// If the given string is empty, the returned list will be empty.
std::vector<GracefulRebootReason> FromFileContent(const std::string reasons) {
  if (reasons == kNoReasons) {
    return {};
  }

  const std::vector<std::string_view> reason_strings =
      fxl::SplitString(reasons, kDeliminator, fxl::WhiteSpaceHandling::kTrimWhitespace,
                       fxl::SplitResult::kSplitWantNonEmpty);
  std::vector<GracefulRebootReason> graceful_reasons;
  graceful_reasons.reserve(reason_strings.size());
  for (const auto& reason : reason_strings) {
    graceful_reasons.push_back(FromString(reason));
  }
  return graceful_reasons;
}

GracefulRebootReason FromReason(
    const fuchsia::hardware::power::statecontrol::RebootReason2& reason) {
  using fuchsia::hardware::power::statecontrol::RebootReason2;
  switch (reason) {
    case RebootReason2::USER_REQUEST:
      return GracefulRebootReason::kUserRequest;
    case RebootReason2::SYSTEM_UPDATE:
      return GracefulRebootReason::kSystemUpdate;
    case RebootReason2::RETRY_SYSTEM_UPDATE:
      return GracefulRebootReason::kRetrySystemUpdate;
    case RebootReason2::HIGH_TEMPERATURE:
      return GracefulRebootReason::kHighTemperature;
    case RebootReason2::SESSION_FAILURE:
      return GracefulRebootReason::kSessionFailure;
    case RebootReason2::SYSMGR_FAILURE:
      return GracefulRebootReason::kSysmgrFailure;
    case RebootReason2::CRITICAL_COMPONENT_FAILURE:
      return GracefulRebootReason::kCriticalComponentFailure;
    case RebootReason2::FACTORY_DATA_RESET:
      return GracefulRebootReason::kFdr;
    case RebootReason2::ZBI_SWAP:
      return GracefulRebootReason::kZbiSwap;
    case RebootReason2::OUT_OF_MEMORY:
      return GracefulRebootReason::kOutOfMemory;
    case RebootReason2::NETSTACK_MIGRATION:
      return GracefulRebootReason::kNetstackMigration;
    default:
      return GracefulRebootReason::kNotSupported;
  }
}

std::vector<GracefulRebootReason> ToGracefulRebootReasons(
    const fuchsia::hardware::power::statecontrol::RebootOptions options) {
  if (!options.has_reasons()) {
    return {};
  }

  std::vector<GracefulRebootReason> reasons;
  reasons.reserve(options.reasons().size());
  for (const auto& reason : options.reasons()) {
    reasons.push_back(FromReason(reason));
  }
  return reasons;
}

void WriteGracefulRebootReasons(const std::vector<GracefulRebootReason>& reasons,
                                cobalt::Logger* cobalt, const std::string& path) {
  fbl::unique_fd fd(open(path.c_str(), O_CREAT | O_TRUNC | O_WRONLY, S_IRUSR | S_IWUSR));
  if (!fd.is_valid()) {
    FX_LOGS(INFO) << "Failed to open reboot reason file: " << path;
    return;
  }

  if (const std::string content = ToFileContent(reasons);
      !fxl::WriteFileDescriptor(fd.get(), content.data(), content.size())) {
    FX_LOGS(ERROR) << "Failed to write reboot reason '" << content << "' to " << path;
  }

  // Force the flush as we want to persist the content asap and we don't have more content to
  // write.
  fsync(fd.get());
}

}  // namespace feedback
}  // namespace forensics
