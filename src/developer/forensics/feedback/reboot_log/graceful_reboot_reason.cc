// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/feedback/reboot_log/graceful_reboot_reason.h"

#include <fcntl.h>
#include <lib/syslog/cpp/macros.h>

#include <string>

#include <fbl/unique_fd.h>

#include "src/lib/files/file_descriptor.h"

namespace forensics {
namespace feedback {
namespace {

constexpr char kReasonNotSet[] = "NOT SET";
constexpr char kReasonNone[] = "NONE";
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
constexpr char kReasonNotSupported[] = "NOT SUPPORTED";
constexpr char kReasonNotParseable[] = "NOT PARSEABLE";

}  // namespace

std::string ToString(const GracefulRebootReason reason) {
  switch (reason) {
    case GracefulRebootReason::kNotSet:
      return kReasonNotSet;
    case GracefulRebootReason::kNone:
      return kReasonNone;
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
    case GracefulRebootReason::kNotSupported:
      return kReasonNotSupported;
    case GracefulRebootReason::kNotParseable:
      return kReasonNotParseable;
  }

  return kReasonNotSet;
}

std::string ToFileContent(const GracefulRebootReason reason) {
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
    case GracefulRebootReason::kNotSupported:
      return ToString(reason);
    case GracefulRebootReason::kNotSet:
    case GracefulRebootReason::kNone:
    case GracefulRebootReason::kNotParseable:
      FX_LOGS(ERROR) << "Invalid persisted graceful reboot reason: " << ToString(reason);
      return kReasonNotSupported;
  }

  return kReasonNotSupported;
}

GracefulRebootReason FromFileContent(const std::string reason) {
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
  } else if (reason == kReasonNotSupported) {
    return GracefulRebootReason::kNotSupported;
  } else if (reason == kOutOfMemory) {
    return GracefulRebootReason::kOutOfMemory;
  }

  FX_LOGS(ERROR) << "Invalid persisted graceful reboot reason: " << reason;
  return GracefulRebootReason::kNotParseable;
}

GracefulRebootReason ToGracefulRebootReason(
    const fuchsia::hardware::power::statecontrol::RebootOptions options) {
  using fuchsia::hardware::power::statecontrol::RebootReason2;

  if (!options.has_reasons() || options.reasons().empty()) {
    return GracefulRebootReason::kNotSupported;
  }

  // TODO(https://fxbug.dev/385734112): Consider multiple reboot reasons. For
  // now, the naive thing is to just check the first.
  const RebootReason2 first_reason = options.reasons()[0];

  switch (first_reason) {
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
      // TODO(https://fxbug.dev/385734112): Add a graceful reboot reason
      // associated with Netstack Migration.
      return GracefulRebootReason::kNotSupported;
    default:
      return GracefulRebootReason::kNotSupported;
  }
}

void WriteGracefulRebootReason(GracefulRebootReason reason, cobalt::Logger* cobalt,
                               const std::string& path) {
  fbl::unique_fd fd(open(path.c_str(), O_CREAT | O_TRUNC | O_WRONLY, S_IRUSR | S_IWUSR));
  if (!fd.is_valid()) {
    FX_LOGS(INFO) << "Failed to open reboot reason file: " << path;
    return;
  }

  if (const std::string content = ToFileContent(reason);
      !fxl::WriteFileDescriptor(fd.get(), content.data(), content.size())) {
    FX_LOGS(ERROR) << "Failed to write reboot reason '" << content << "' to " << path;
  }

  // Force the flush as we want to persist the content asap and we don't have more content to
  // write.
  fsync(fd.get());
}

}  // namespace feedback
}  // namespace forensics
