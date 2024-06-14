// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/testing/stubs/power_broker_lease_control.h"

namespace forensics::stubs {

void PowerBrokerLeaseControl::WatchStatus(WatchStatusRequest& request,
                                          WatchStatusCompleter::Sync& completer) {
  FX_CHECK(!queued_response_.has_value());

  if (request.last_status() == fuchsia_power_broker::LeaseStatus::kUnknown ||
      request.last_status() != current_status_) {
    completer.Reply(current_status_);
    return;
  }

  queued_response_ = completer.ToAsync();
}

void PowerBrokerLeaseControl::SetStatus(fuchsia_power_broker::LeaseStatus status) {
  FX_CHECK(status != current_status_);

  current_status_ = status;
  queued_response_->Reply(current_status_);
  queued_response_ = std::nullopt;
}

}  // namespace forensics::stubs
