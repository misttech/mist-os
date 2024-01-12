// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/feedback/annotations/time_provider.h"

#include <fuchsia/time/cpp/fidl.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/time.h>

#include <string>

#include "src/developer/forensics/feedback/annotations/constants.h"
#include "src/developer/forensics/utils/errors.h"
#include "src/developer/forensics/utils/time.h"

namespace forensics::feedback {
namespace {

ErrorOrString GetUptime() {
  const auto uptime = FormatDuration(zx::nsec(zx_clock_get_monotonic()));
  if (!uptime) {
    FX_LOGS(ERROR) << "Got negative uptime from zx_clock_get_monotonic()";
    return ErrorOrString(Error::kBadValue);
  }

  return ErrorOrString(*uptime);
}

}  // namespace

TimeProvider::TimeProvider(async_dispatcher_t* dispatcher, zx::unowned_clock clock_handle,
                           std::unique_ptr<timekeeper::Clock> clock)
    : clock_(std::move(clock)),
      wait_for_logging_quality_clock_(this, clock_handle->get_handle(),
                                      fuchsia::time::SIGNAL_UTC_CLOCK_LOGGING_QUALITY,
                                      /*options=*/0) {
  if (const zx_status_t status = wait_for_logging_quality_clock_.Begin(dispatcher);
      status != ZX_OK) {
    FX_PLOGS(FATAL, status) << "Failed to wait for logging quality clock";
  }
}

std::set<std::string> TimeProvider::GetKeys() const {
  return {
      kDeviceUptimeKey,
      kDeviceUtcTimeKey,
  };
}

Annotations TimeProvider::Get() {
  const auto utc_time = [this]() -> ErrorOrString {
    if (is_utc_time_accurate_) {
      return ErrorOrString(CurrentUtcTime(clock_.get()));
    }

    return ErrorOrString(Error::kMissingValue);
  }();

  return {
      {kDeviceUptimeKey, GetUptime()},
      {kDeviceUtcTimeKey, utc_time},
  };
}

void TimeProvider::OnClockLoggingQuality(async_dispatcher_t* dispatcher, async::WaitBase* wait,
                                         zx_status_t status, const zx_packet_signal_t* signal) {
  if (status != ZX_OK) {
    FX_PLOGS(WARNING, status)
        << "Wait for logging quality clock completed with error, trying again";

    // Attempt to wait for the clock to achieve logging quality again.
    wait->Begin(dispatcher);
    return;
  }

  is_utc_time_accurate_ = true;
}

}  // namespace forensics::feedback
