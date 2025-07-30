// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/testing/mock-fidl-engine.h"

#include <fidl/fuchsia.hardware.display.engine/cpp/driver/wire.h>
#include <zircon/assert.h>

#include <mutex>
#include <utility>

namespace display_coordinator::testing {

// Exactly one of the members is non-null.
struct MockFidlEngine::Expectation {
  CompleteCoordinatorConnectionChecker complete_coordinator_connection_checker;
  ImportBufferCollectionChecker import_buffer_collection_checker;
  ReleaseBufferCollectionChecker release_buffer_collection_checker;
  ImportImageChecker import_image_checker;
  ImportImageForCaptureChecker import_image_for_capture_checker;
  ReleaseImageChecker release_image_checker;
  CheckConfigurationChecker check_configuration_checker;
  ApplyConfigurationChecker apply_configuration_checker;
  SetBufferCollectionConstraintsChecker set_buffer_collection_constraints_checker;
  SetDisplayPowerChecker set_display_power_checker;
  SetMinimumRgbChecker set_minimum_rgb_checker;
  StartCaptureChecker start_capture_checker;
  ReleaseCaptureChecker release_capture_checker;
  IsAvailableChecker is_available_checker;
};

MockFidlEngine::MockFidlEngine() = default;

MockFidlEngine::~MockFidlEngine() {
  ZX_ASSERT_MSG(check_all_calls_replayed_called_, "CheckAllCallsReplayed() not called on a mock");
}

void MockFidlEngine::ExpectCompleteCoordinatorConnection(
    CompleteCoordinatorConnectionChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back({.complete_coordinator_connection_checker = std::move(checker)});
}

void MockFidlEngine::ExpectImportBufferCollection(ImportBufferCollectionChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back({.import_buffer_collection_checker = std::move(checker)});
}

void MockFidlEngine::ExpectReleaseBufferCollection(ReleaseBufferCollectionChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back({.release_buffer_collection_checker = std::move(checker)});
}

void MockFidlEngine::ExpectImportImage(ImportImageChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back({.import_image_checker = std::move(checker)});
}

void MockFidlEngine::ExpectImportImageForCapture(ImportImageForCaptureChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back({.import_image_for_capture_checker = std::move(checker)});
}

void MockFidlEngine::ExpectReleaseImage(ReleaseImageChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back({.release_image_checker = std::move(checker)});
}

void MockFidlEngine::ExpectCheckConfiguration(CheckConfigurationChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back({.check_configuration_checker = std::move(checker)});
}

void MockFidlEngine::ExpectApplyConfiguration(ApplyConfigurationChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back({.apply_configuration_checker = std::move(checker)});
}

void MockFidlEngine::ExpectSetBufferCollectionConstraints(
    SetBufferCollectionConstraintsChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back({.set_buffer_collection_constraints_checker = std::move(checker)});
}

void MockFidlEngine::ExpectSetDisplayPower(SetDisplayPowerChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back({.set_display_power_checker = std::move(checker)});
}

void MockFidlEngine::ExpectSetMinimumRgb(SetMinimumRgbChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back({.set_minimum_rgb_checker = std::move(checker)});
}

void MockFidlEngine::ExpectStartCapture(StartCaptureChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back({.start_capture_checker = std::move(checker)});
}

void MockFidlEngine::ExpectReleaseCapture(ReleaseCaptureChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back({.release_capture_checker = std::move(checker)});
}

void MockFidlEngine::ExpectIsAvailable(IsAvailableChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back({.is_available_checker = std::move(checker)});
}

void MockFidlEngine::CheckAllCallsReplayed() {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(expectations_.size() == call_index_, "%zu expected calls were not received",
                expectations_.size() - call_index_);
  check_all_calls_replayed_called_ = true;
}

void MockFidlEngine::CompleteCoordinatorConnection(
    fuchsia_hardware_display_engine::wire::EngineCompleteCoordinatorConnectionRequest* request,
    fdf::Arena& arena, CompleteCoordinatorConnectionCompleter::Sync& completer) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;

  ZX_ASSERT_MSG(call_expectation.complete_coordinator_connection_checker != nullptr,
                "Received call type does not match expected call type");
  call_expectation.complete_coordinator_connection_checker(request, arena, completer);
}

void MockFidlEngine::ImportBufferCollection(
    fuchsia_hardware_display_engine::wire::EngineImportBufferCollectionRequest* request,
    fdf::Arena& arena, ImportBufferCollectionCompleter::Sync& completer) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;

  ZX_ASSERT_MSG(call_expectation.import_buffer_collection_checker != nullptr,
                "Received call type does not match expected call type");
  call_expectation.import_buffer_collection_checker(request, arena, completer);
}

void MockFidlEngine::ReleaseBufferCollection(
    fuchsia_hardware_display_engine::wire::EngineReleaseBufferCollectionRequest* request,
    fdf::Arena& arena, ReleaseBufferCollectionCompleter::Sync& completer) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;

  ZX_ASSERT_MSG(call_expectation.release_buffer_collection_checker != nullptr,
                "Received call type does not match expected call type");
  call_expectation.release_buffer_collection_checker(request, arena, completer);
}

void MockFidlEngine::ImportImage(
    fuchsia_hardware_display_engine::wire::EngineImportImageRequest* request, fdf::Arena& arena,
    ImportImageCompleter::Sync& completer) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;

  ZX_ASSERT_MSG(call_expectation.import_image_checker != nullptr,
                "Received call type does not match expected call type");
  call_expectation.import_image_checker(request, arena, completer);
}

void MockFidlEngine::ImportImageForCapture(
    fuchsia_hardware_display_engine::wire::EngineImportImageForCaptureRequest* request,
    fdf::Arena& arena, ImportImageForCaptureCompleter::Sync& completer) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;

  ZX_ASSERT_MSG(call_expectation.import_image_for_capture_checker != nullptr,
                "Received call type does not match expected call type");
  call_expectation.import_image_for_capture_checker(request, arena, completer);
}

void MockFidlEngine::ReleaseImage(
    fuchsia_hardware_display_engine::wire::EngineReleaseImageRequest* request, fdf::Arena& arena,
    ReleaseImageCompleter::Sync& completer) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;

  ZX_ASSERT_MSG(call_expectation.release_image_checker != nullptr,
                "Received call type does not match expected call type");
  call_expectation.release_image_checker(request, arena);
}

void MockFidlEngine::CheckConfiguration(
    fuchsia_hardware_display_engine::wire::EngineCheckConfigurationRequest* request,
    fdf::Arena& arena, CheckConfigurationCompleter::Sync& completer) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;

  ZX_ASSERT_MSG(call_expectation.check_configuration_checker != nullptr,
                "Received call type does not match expected call type");
  call_expectation.check_configuration_checker(request, arena, completer);
}

void MockFidlEngine::ApplyConfiguration(
    fuchsia_hardware_display_engine::wire::EngineApplyConfigurationRequest* request,
    fdf::Arena& arena, ApplyConfigurationCompleter::Sync& completer) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;

  ZX_ASSERT_MSG(call_expectation.apply_configuration_checker != nullptr,
                "Received call type does not match expected call type");
  call_expectation.apply_configuration_checker(request, arena, completer);
}

void MockFidlEngine::SetBufferCollectionConstraints(
    fuchsia_hardware_display_engine::wire::EngineSetBufferCollectionConstraintsRequest* request,
    fdf::Arena& arena, SetBufferCollectionConstraintsCompleter::Sync& completer) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;

  ZX_ASSERT_MSG(call_expectation.set_buffer_collection_constraints_checker != nullptr,
                "Received call type does not match expected call type");
  call_expectation.set_buffer_collection_constraints_checker(request, arena, completer);
}

void MockFidlEngine::SetDisplayPower(
    fuchsia_hardware_display_engine::wire::EngineSetDisplayPowerRequest* request, fdf::Arena& arena,
    SetDisplayPowerCompleter::Sync& completer) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;

  ZX_ASSERT_MSG(call_expectation.set_display_power_checker != nullptr,
                "Received call type does not match expected call type");
  call_expectation.set_display_power_checker(request, arena, completer);
}

void MockFidlEngine::SetMinimumRgb(
    fuchsia_hardware_display_engine::wire::EngineSetMinimumRgbRequest* request, fdf::Arena& arena,
    SetMinimumRgbCompleter::Sync& completer) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;

  ZX_ASSERT_MSG(call_expectation.set_minimum_rgb_checker != nullptr,
                "Received call type does not match expected call type");
  call_expectation.set_minimum_rgb_checker(request, arena, completer);
}

void MockFidlEngine::StartCapture(
    fuchsia_hardware_display_engine::wire::EngineStartCaptureRequest* request, fdf::Arena& arena,
    StartCaptureCompleter::Sync& completer) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;

  ZX_ASSERT_MSG(call_expectation.start_capture_checker != nullptr,
                "Received call type does not match expected call type");
  call_expectation.start_capture_checker(request, arena, completer);
}

void MockFidlEngine::ReleaseCapture(
    fuchsia_hardware_display_engine::wire::EngineReleaseCaptureRequest* request, fdf::Arena& arena,
    ReleaseCaptureCompleter::Sync& completer) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;

  ZX_ASSERT_MSG(call_expectation.release_capture_checker != nullptr,
                "Received call type does not match expected call type");
  call_expectation.release_capture_checker(request, arena, completer);
}

void MockFidlEngine::IsAvailable(fdf::Arena& arena, IsAvailableCompleter::Sync& completer) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(call_index_ < expectations_.size(), "All expected calls were already received");
  Expectation& call_expectation = expectations_[call_index_];
  ++call_index_;

  ZX_ASSERT_MSG(call_expectation.is_available_checker != nullptr,
                "Received call type does not match expected call type");
  call_expectation.is_available_checker(arena, completer);
}

void MockFidlEngine::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_display_engine::Engine> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  ZX_PANIC("Received unknown FIDL method call");
}

}  // namespace display_coordinator::testing
