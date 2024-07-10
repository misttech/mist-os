// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/testing/stubs/power_broker_required_level.h"

namespace forensics::stubs {

void PowerBrokerRequiredLevel::Watch(WatchCompleter::Sync& completer) {
  FX_CHECK(!queued_response_.has_value());

  if (level_changed_) {
    completer.Reply(fit::ok(level_));
    level_changed_ = false;
    return;
  }

  queued_response_ = completer.ToAsync();
}

void PowerBrokerRequiredLevel::SetLevel(const uint8_t level) {
  level_ = level;

  if (!queued_response_.has_value()) {
    level_changed_ = true;
    return;
  }

  queued_response_->Reply(fit::ok(level_));
  queued_response_ = std::nullopt;
}

}  // namespace forensics::stubs
