// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-protocols/cpp/mock-banjo-display-engine-listener.h"

#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <zircon/assert.h>

#include <cstdint>
#include <mutex>

namespace display::testing {

// Exactly one of the members is non-null.
struct MockBanjoDisplayEngineListener::Expectation {
  OnDisplayAddedChecker on_display_added_checker;
  OnDisplayRemovedChecker on_display_removed_checker;
  OnDisplayVsyncChecker on_display_vsync_checker;
  OnCaptureCompleteChecker on_capture_complete_checker;
};

MockBanjoDisplayEngineListener::MockBanjoDisplayEngineListener() = default;

MockBanjoDisplayEngineListener::~MockBanjoDisplayEngineListener() {
  ZX_ASSERT_MSG(check_all_calls_replayed_called_, "CheckAllCallsReplayed() not called on a mock");
}

void MockBanjoDisplayEngineListener::ExpectOnDisplayAdded(OnDisplayAddedChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back({.on_display_added_checker = std::move(checker)});
}

void MockBanjoDisplayEngineListener::ExpectOnDisplayRemoved(OnDisplayRemovedChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back(Expectation{.on_display_removed_checker = std::move(checker)});
}

void MockBanjoDisplayEngineListener::ExpectOnDisplayVsync(OnDisplayVsyncChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back(Expectation{.on_display_vsync_checker = std::move(checker)});
}

void MockBanjoDisplayEngineListener::ExpectOnCaptureComplete(OnCaptureCompleteChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back(Expectation{.on_capture_complete_checker = std::move(checker)});
}

void MockBanjoDisplayEngineListener::CheckAllCallsReplayed() {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(expectations_.size() == call_index_, "%zu expected calls were not received",
                expectations_.size() - call_index_);
  check_all_calls_replayed_called_ = true;
}

display_engine_listener_protocol_t MockBanjoDisplayEngineListener::GetProtocol() {
  return display_engine_listener_protocol_t{
      .ops = &display_engine_listener_protocol_ops_,
      .ctx = this,
  };
}

void MockBanjoDisplayEngineListener::DisplayEngineListenerOnDisplayAdded(
    const raw_display_info_t* banjo_raw_display_info) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;

  ZX_ASSERT_MSG(call_expectation.on_display_added_checker != nullptr,
                "Received call type does not match expected call type");
  call_expectation.on_display_added_checker(banjo_raw_display_info);
}

void MockBanjoDisplayEngineListener::DisplayEngineListenerOnDisplayRemoved(
    uint64_t banjo_display_id) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;

  ZX_ASSERT_MSG(call_expectation.on_display_removed_checker != nullptr,
                "Received call type does not match expected call type");
  call_expectation.on_display_removed_checker(banjo_display_id);
}

void MockBanjoDisplayEngineListener::DisplayEngineListenerOnDisplayVsync(
    uint64_t banjo_display_id, zx_time_t banjo_timestamp,
    const config_stamp_t* banjo_config_stamp) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;

  ZX_ASSERT_MSG(call_expectation.on_display_vsync_checker != nullptr,
                "Received call type does not match expected call type");
  call_expectation.on_display_vsync_checker(banjo_display_id, banjo_timestamp, banjo_config_stamp);
}

void MockBanjoDisplayEngineListener::DisplayEngineListenerOnCaptureComplete() {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;

  ZX_ASSERT_MSG(call_expectation.on_capture_complete_checker != nullptr,
                "Received call type does not match expected call type");
  call_expectation.on_capture_complete_checker();
}

}  // namespace display::testing
