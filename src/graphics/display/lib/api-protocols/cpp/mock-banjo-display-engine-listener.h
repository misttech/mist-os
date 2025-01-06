// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_PROTOCOLS_CPP_MOCK_BANJO_DISPLAY_ENGINE_LISTENER_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_PROTOCOLS_CPP_MOCK_BANJO_DISPLAY_ENGINE_LISTENER_H_

#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/fit/function.h>
#include <zircon/compiler.h>

#include <cstddef>
#include <cstdint>
#include <mutex>
#include <vector>

namespace display::testing {

// Strict mock for the Banjo-generated DisplayEngineListener protocol.
//
// This is a very rare case where strict mocking is warranted. The code under
// test is an adapter that maps C++ calls 1:1 to Banjo calls. So, the API
// contract being tested is expressed in terms of individual function calls.
class MockBanjoDisplayEngineListener
    : public ddk::DisplayEngineListenerProtocol<MockBanjoDisplayEngineListener> {
 public:
  // Expectation containers for fuchsia.hardware.display.controller/DisplayEngineListener:
  using OnDisplayAddedChecker =
      fit::function<void(const raw_display_info_t* banjo_raw_display_info)>;
  using OnDisplayRemovedChecker = fit::function<void(uint64_t banjo_display_id)>;
  using OnDisplayVsyncChecker =
      fit::function<void(uint64_t banjo_display_id, zx_time_t banjo_timestamp,
                         const config_stamp_t* banjo_config_stamp)>;
  using OnCaptureCompleteChecker = fit::function<void()>;

  MockBanjoDisplayEngineListener();
  MockBanjoDisplayEngineListener(const MockBanjoDisplayEngineListener&) = delete;
  MockBanjoDisplayEngineListener& operator=(const MockBanjoDisplayEngineListener&) = delete;
  ~MockBanjoDisplayEngineListener();

  // Expectations for fuchsia.hardware.display.controller/DisplayEngineListener:
  void ExpectOnDisplayAdded(OnDisplayAddedChecker checker);
  void ExpectOnDisplayRemoved(OnDisplayRemovedChecker checker);
  void ExpectOnDisplayVsync(OnDisplayVsyncChecker checker);
  void ExpectOnCaptureComplete(OnCaptureCompleteChecker checker);

  // Must be called at least once during an instance's lifetime.
  //
  // Tests are recommended to call this in a TearDown() method, or at the end of
  // the test case implementation.
  void CheckAllCallsReplayed();

  // The returned value lives as long as this instance.
  display_engine_listener_protocol_t GetProtocol();

  // fuchsia.hardware.display.controller/DisplayEngineListener:
  void DisplayEngineListenerOnDisplayAdded(const raw_display_info_t* banjo_raw_display_info);
  void DisplayEngineListenerOnDisplayRemoved(uint64_t banjo_display_id);
  void DisplayEngineListenerOnDisplayVsync(uint64_t banjo_display_id, zx_time_t banjo_timestamp,
                                           const config_stamp_t* banjo_config_stamp);
  void DisplayEngineListenerOnCaptureComplete();

 private:
  struct Expectation;

  std::mutex mutex_;
  std::vector<Expectation> expectations_ __TA_GUARDED(mutex_);
  size_t call_index_ __TA_GUARDED(mutex_) = 0;
  bool check_all_calls_replayed_called_ __TA_GUARDED(mutex_) = false;
};

}  // namespace display::testing

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_PROTOCOLS_CPP_MOCK_BANJO_DISPLAY_ENGINE_LISTENER_H_
