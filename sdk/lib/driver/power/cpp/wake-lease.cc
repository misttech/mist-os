// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/logging/cpp/logger.h>
#include <lib/driver/power/cpp/wake-lease.h>
#include <lib/zx/clock.h>

#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)

namespace fdf_power {

WakeLease::WakeLease(async_dispatcher_t* dispatcher, std::string_view lease_name,
                     fidl::ClientEnd<fuchsia_power_system::ActivityGovernor> sag_client,
                     inspect::Node* parent_node, bool log)
    : dispatcher_(dispatcher), lease_name_(lease_name), log_(log) {
  if (sag_client) {
    sag_client_.Bind(std::move(sag_client));
  }

  if (parent_node) {
    total_lease_acquisitions_ = parent_node->CreateUint("Total Lease Acquisitions", 0);
    wake_lease_held_ = parent_node->CreateBool("Wake Lease Held", false);
    wake_lease_grabbable_ = parent_node->CreateBool("Wake Lease Grabbable", sag_client_.is_valid());
    wake_lease_last_acquired_timestamp_ =
        parent_node->CreateUint("Wake Lease Last Acquired Timestamp (ns)", 0);
    wake_lease_last_refreshed_timestamp_ =
        parent_node->CreateUint("Wake Lease Last Refreshed Timestamp (ns)", 0);
  }
}

bool WakeLease::AcquireWakeLease(zx::duration timeout) {
  if (!sag_client_) {
    return false;
  }

  if (lease_) {
    // If already holding a lease, cancel the current timeout.
    lease_task_.Cancel();
    wake_lease_last_refreshed_timestamp_.Set(zx::clock::get_monotonic().get());
  } else {
    // If not holding a lease, take one.
    auto result_lease = sag_client_->TakeWakeLease(fidl::StringView::FromExternal(lease_name_));
    if (!result_lease.ok()) {
      if (log_) {
        FDF_LOG(
            WARNING,
            "Failed to take wake lease, system may incorrectly enter suspend: %s. Will not attempt again.",
            result_lease.status_string());
      }
      sag_client_ = {};
      wake_lease_grabbable_.Set(false);
      return false;
    }

    lease_ = std::move(result_lease->token);
    if (log_) {
      FDF_LOG(INFO, "Created a wake lease due to recent wake event.");
    }
    auto now = zx::clock::get_monotonic().get();
    wake_lease_last_acquired_timestamp_.Set(now);
    wake_lease_last_refreshed_timestamp_.Set(now);
    total_lease_acquisitions_.Add(1);
    wake_lease_held_.Set(true);
  }

  lease_task_.PostDelayed(dispatcher_, timeout);
  return true;
}

zx::eventpair WakeLease::TakeWakeLease() {
  lease_task_.Cancel();
  return std::move(lease_);
}

void WakeLease::HandleTimeout() {
  if (log_) {
    FDF_LOG(INFO, "Dropping the wake lease due to not receiving any wake events.");
  }
  lease_.reset();
  wake_lease_held_.Set(false);
}

}  // namespace fdf_power

#endif
