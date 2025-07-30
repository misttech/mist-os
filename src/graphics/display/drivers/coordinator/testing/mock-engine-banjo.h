// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_TESTING_MOCK_ENGINE_BANJO_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_TESTING_MOCK_ENGINE_BANJO_H_

#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/fit/function.h>
#include <zircon/compiler.h>

#include <cstddef>
#include <cstdint>
#include <mutex>
#include <vector>

namespace display_coordinator::testing {

// Strict mock for the FIDL-generated display Engine protocol.
//
// This is a very rare case where strict mocking is warranted. The code under
// test is an adapter that maps C++ calls 1:1 to FIDL calls. So, the API
// contract being tested is expressed in terms of individual function calls.
class MockEngineBanjo : public ddk::DisplayEngineProtocol<MockEngineBanjo> {
 public:
  // Expectation containers for fuchsia.hardware.display.controller/DisplayEngine:
  using ReleaseImageChecker = fit::function<void(uint64_t image_handle)>;
  using ReleaseCaptureChecker = fit::function<zx_status_t(uint64_t capture_handle)>;
  using CheckConfigurationChecker =
      fit::function<config_check_result_t(const display_config_t* display_config)>;
  using ApplyConfigurationChecker = fit::function<void(const display_config_t* display_config,
                                                       const config_stamp_t* config_stamp)>;
  using CompleteCoordinatorConnectionChecker = fit::function<void(
      const display_engine_listener_protocol_t* listener_protocol, engine_info_t* out_info)>;
  using UnsetListenerChecker = fit::function<void()>;
  using ImportImageChecker =
      fit::function<zx_status_t(const image_metadata_t* image_metadata, uint64_t collection_id,
                                uint32_t index, uint64_t* out_image_handle)>;
  using ImportImageForCaptureChecker = fit::function<zx_status_t(
      uint64_t collection_id, uint32_t index, uint64_t* out_capture_handle)>;
  using ImportBufferCollectionChecker =
      fit::function<zx_status_t(uint64_t collection_id, zx::channel collection_token)>;
  using ReleaseBufferCollectionChecker = fit::function<zx_status_t(uint64_t collection_id)>;
  using SetBufferCollectionConstraintsChecker =
      fit::function<zx_status_t(const image_buffer_usage_t* usage, uint64_t collection_id)>;
  using StartCaptureChecker = fit::function<zx_status_t(uint64_t capture_handle)>;
  using SetDisplayPowerChecker = fit::function<zx_status_t(uint64_t display_id, bool power_on)>;
  using SetMinimumRgbChecker = fit::function<zx_status_t(uint8_t minimum_rgb)>;

  MockEngineBanjo();
  MockEngineBanjo(const MockEngineBanjo&) = delete;
  MockEngineBanjo& operator=(const MockEngineBanjo&) = delete;
  ~MockEngineBanjo();

  // Expectations for fuchsia.hardware.display.controller/DisplayEngine:
  void ExpectCompleteCoordinatorConnection(CompleteCoordinatorConnectionChecker checker);
  void ExpectUnsetListener(UnsetListenerChecker checker);
  void ExpectImportBufferCollection(ImportBufferCollectionChecker checker);
  void ExpectReleaseBufferCollection(ReleaseBufferCollectionChecker checker);
  void ExpectImportImage(ImportImageChecker checker);
  void ExpectImportImageForCapture(ImportImageForCaptureChecker checker);
  void ExpectReleaseImage(ReleaseImageChecker checker);
  void ExpectCheckConfiguration(CheckConfigurationChecker checker);
  void ExpectApplyConfiguration(ApplyConfigurationChecker checker);
  void ExpectSetBufferCollectionConstraints(SetBufferCollectionConstraintsChecker checker);
  void ExpectSetDisplayPower(SetDisplayPowerChecker checker);
  void ExpectStartCapture(StartCaptureChecker checker);
  void ExpectReleaseCapture(ReleaseCaptureChecker checker);
  void ExpectSetMinimumRgb(SetMinimumRgbChecker checker);

  // Must be called at least once during an instance's lifetime.
  //
  // Tests are recommended to call this in a TearDown() method, or at the end of
  // the test case implementation.
  void CheckAllCallsReplayed();

  // The returned value lives as long as this instance.
  display_engine_protocol_t GetProtocol();

  // ddk::DisplayEngineProtocol:
  void DisplayEngineCompleteCoordinatorConnection(
      const display_engine_listener_protocol_t* listener_protocol, engine_info_t* out_info);
  void DisplayEngineUnsetListener();
  zx_status_t DisplayEngineImportBufferCollection(uint64_t collection_id,
                                                  zx::channel collection_token);
  zx_status_t DisplayEngineReleaseBufferCollection(uint64_t collection_id);
  zx_status_t DisplayEngineImportImage(const image_metadata_t* image_metadata,
                                       uint64_t collection_id, uint32_t index,
                                       uint64_t* out_image_handle);
  zx_status_t DisplayEngineImportImageForCapture(uint64_t collection_id, uint32_t index,
                                                 uint64_t* out_capture_handle);
  void DisplayEngineReleaseImage(uint64_t image_handle);
  config_check_result_t DisplayEngineCheckConfiguration(const display_config_t* display_config);
  void DisplayEngineApplyConfiguration(const display_config_t* display_config,
                                       const config_stamp_t* config_stamp);
  zx_status_t DisplayEngineSetBufferCollectionConstraints(const image_buffer_usage_t* usage,
                                                          uint64_t collection_id);
  zx_status_t DisplayEngineSetDisplayPower(uint64_t display_id, bool power_on);
  zx_status_t DisplayEngineStartCapture(uint64_t capture_handle);
  zx_status_t DisplayEngineReleaseCapture(uint64_t capture_handle);
  zx_status_t DisplayEngineSetMinimumRgb(uint8_t minimum_rgb);

 private:
  struct Expectation;

  std::mutex mutex_;
  std::vector<Expectation> expectations_ __TA_GUARDED(mutex_);
  size_t call_index_ __TA_GUARDED(mutex_) = 0;
  bool check_all_calls_replayed_called_ __TA_GUARDED(mutex_) = false;
};

}  // namespace display_coordinator::testing

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_TESTING_MOCK_ENGINE_BANJO_H_
