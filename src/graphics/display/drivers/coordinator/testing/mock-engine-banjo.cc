// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/testing/mock-engine-banjo.h"

#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <zircon/assert.h>

#include <cstdint>
#include <mutex>
#include <utility>

namespace display_coordinator::testing {

// Exactly one of the members is non-null.
struct MockEngineBanjo::Expectation {
  CompleteCoordinatorConnectionChecker complete_coordinator_connection_checker;
  UnsetListenerChecker unset_listener_checker;
  ImportBufferCollectionChecker import_buffer_collection_checker;
  ReleaseBufferCollectionChecker release_buffer_collection_checker;
  ImportImageChecker import_image_checker;
  ImportImageForCaptureChecker import_image_for_capture_checker;
  ReleaseImageChecker release_image_checker;
  ReleaseCaptureChecker release_capture_checker;
  CheckConfigurationChecker check_configuration_checker;
  ApplyConfigurationChecker apply_configuration_checker;
  SetBufferCollectionConstraintsChecker set_buffer_collection_constraints_checker;
  StartCaptureChecker start_capture_checker;
  SetDisplayPowerChecker set_display_power_checker;
  SetMinimumRgbChecker set_minimum_rgb_checker;
};

MockEngineBanjo::MockEngineBanjo() = default;

MockEngineBanjo::~MockEngineBanjo() {
  ZX_ASSERT_MSG(check_all_calls_replayed_called_, "CheckAllCallsReplayed() not called on a mock");
}

void MockEngineBanjo::ExpectCompleteCoordinatorConnection(
    CompleteCoordinatorConnectionChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back({.complete_coordinator_connection_checker = std::move(checker)});
}

void MockEngineBanjo::ExpectUnsetListener(UnsetListenerChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back({.unset_listener_checker = std::move(checker)});
}

void MockEngineBanjo::ExpectImportBufferCollection(ImportBufferCollectionChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back({.import_buffer_collection_checker = std::move(checker)});
}

void MockEngineBanjo::ExpectReleaseBufferCollection(ReleaseBufferCollectionChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back({.release_buffer_collection_checker = std::move(checker)});
}

void MockEngineBanjo::ExpectImportImage(ImportImageChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back({.import_image_checker = std::move(checker)});
}

void MockEngineBanjo::ExpectImportImageForCapture(ImportImageForCaptureChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back({.import_image_for_capture_checker = std::move(checker)});
}

void MockEngineBanjo::ExpectReleaseImage(ReleaseImageChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back({.release_image_checker = std::move(checker)});
}

void MockEngineBanjo::ExpectCheckConfiguration(CheckConfigurationChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back({.check_configuration_checker = std::move(checker)});
}

void MockEngineBanjo::ExpectApplyConfiguration(ApplyConfigurationChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back({.apply_configuration_checker = std::move(checker)});
}

void MockEngineBanjo::ExpectSetBufferCollectionConstraints(
    SetBufferCollectionConstraintsChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back({.set_buffer_collection_constraints_checker = std::move(checker)});
}

void MockEngineBanjo::ExpectSetDisplayPower(SetDisplayPowerChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back({.set_display_power_checker = std::move(checker)});
}

void MockEngineBanjo::ExpectStartCapture(StartCaptureChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back({.start_capture_checker = std::move(checker)});
}

void MockEngineBanjo::ExpectReleaseCapture(ReleaseCaptureChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back({.release_capture_checker = std::move(checker)});
}

void MockEngineBanjo::ExpectSetMinimumRgb(SetMinimumRgbChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back({.set_minimum_rgb_checker = std::move(checker)});
}

void MockEngineBanjo::CheckAllCallsReplayed() {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(expectations_.size() == call_index_, "%zu expected calls were not received",
                expectations_.size() - call_index_);
  check_all_calls_replayed_called_ = true;
}

display_engine_protocol_t MockEngineBanjo::GetProtocol() {
  return display_engine_protocol_t{
      .ops = &display_engine_protocol_ops_,
      .ctx = this,
  };
}

void MockEngineBanjo::DisplayEngineCompleteCoordinatorConnection(
    const display_engine_listener_protocol_t* listener_protocol, engine_info_t* out_info) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;
  ZX_ASSERT_MSG(call_expectation.complete_coordinator_connection_checker,
                "Received call type does not match expected call type");
  call_expectation.complete_coordinator_connection_checker(listener_protocol, out_info);
}

void MockEngineBanjo::DisplayEngineUnsetListener() {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;
  ZX_ASSERT_MSG(call_expectation.unset_listener_checker,
                "Received call type does not match expected call type");
  call_expectation.unset_listener_checker();
}

zx_status_t MockEngineBanjo::DisplayEngineImportBufferCollection(uint64_t collection_id,
                                                                 zx::channel collection_token) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;
  ZX_ASSERT_MSG(call_expectation.import_buffer_collection_checker,
                "Received call type does not match expected call type");
  return call_expectation.import_buffer_collection_checker(collection_id,
                                                           std::move(collection_token));
}

zx_status_t MockEngineBanjo::DisplayEngineReleaseBufferCollection(uint64_t collection_id) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;
  ZX_ASSERT_MSG(call_expectation.release_buffer_collection_checker,
                "Received call type does not match expected call type");
  return call_expectation.release_buffer_collection_checker(collection_id);
}

zx_status_t MockEngineBanjo::DisplayEngineImportImage(const image_metadata_t* image_metadata,
                                                      uint64_t collection_id, uint32_t index,
                                                      uint64_t* out_image_handle) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;
  ZX_ASSERT_MSG(call_expectation.import_image_checker,
                "Received call type does not match expected call type");
  return call_expectation.import_image_checker(image_metadata, collection_id, index,
                                               out_image_handle);
}

zx_status_t MockEngineBanjo::DisplayEngineImportImageForCapture(uint64_t collection_id,
                                                                uint32_t index,
                                                                uint64_t* out_capture_handle) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;
  ZX_ASSERT_MSG(call_expectation.import_image_for_capture_checker,
                "Received call type does not match expected call type");
  return call_expectation.import_image_for_capture_checker(collection_id, index,
                                                           out_capture_handle);
}

void MockEngineBanjo::DisplayEngineReleaseImage(uint64_t image_handle) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;
  ZX_ASSERT_MSG(call_expectation.release_image_checker,
                "Received call type does not match expected call type");
  call_expectation.release_image_checker(image_handle);
}

zx_status_t MockEngineBanjo::DisplayEngineReleaseCapture(uint64_t capture_handle) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;
  ZX_ASSERT_MSG(call_expectation.release_capture_checker,
                "Received call type does not match expected call type");
  return call_expectation.release_capture_checker(capture_handle);
}

config_check_result_t MockEngineBanjo::DisplayEngineCheckConfiguration(
    const display_config_t* display_config) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;
  ZX_ASSERT_MSG(call_expectation.check_configuration_checker,
                "Received call type does not match expected call type");
  return call_expectation.check_configuration_checker(display_config);
}

void MockEngineBanjo::DisplayEngineApplyConfiguration(const display_config_t* display_config,
                                                      const config_stamp_t* config_stamp) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;
  ZX_ASSERT_MSG(call_expectation.apply_configuration_checker,
                "Received call type does not match expected call type");
  call_expectation.apply_configuration_checker(display_config, config_stamp);
}

zx_status_t MockEngineBanjo::DisplayEngineSetBufferCollectionConstraints(
    const image_buffer_usage_t* usage, uint64_t collection_id) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;
  ZX_ASSERT_MSG(call_expectation.set_buffer_collection_constraints_checker,
                "Received call type does not match expected call type");
  return call_expectation.set_buffer_collection_constraints_checker(usage, collection_id);
}

zx_status_t MockEngineBanjo::DisplayEngineStartCapture(uint64_t capture_handle) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;
  ZX_ASSERT_MSG(call_expectation.start_capture_checker,
                "Received call type does not match expected call type");
  return call_expectation.start_capture_checker(capture_handle);
}

zx_status_t MockEngineBanjo::DisplayEngineSetDisplayPower(uint64_t display_id, bool power_on) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;
  ZX_ASSERT_MSG(call_expectation.set_display_power_checker,
                "Received call type does not match expected call type");
  return call_expectation.set_display_power_checker(display_id, power_on);
}

zx_status_t MockEngineBanjo::DisplayEngineSetMinimumRgb(uint8_t minimum_rgb) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;
  ZX_ASSERT_MSG(call_expectation.set_minimum_rgb_checker,
                "Received call type does not match expected call type");
  return call_expectation.set_minimum_rgb_checker(minimum_rgb);
}

}  // namespace display_coordinator::testing
