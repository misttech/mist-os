// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-protocols/cpp/mock-display-engine-for-out-of-tree.h"

#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <lib/fit/function.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/result.h>

#include <cstddef>
#include <list>
#include <mutex>

#include "src/graphics/display/lib/api-types/cpp/config-check-result.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-capture-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/driver-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-layer.h"
#include "src/graphics/display/lib/api-types/cpp/engine-info.h"
#include "src/graphics/display/lib/api-types/cpp/image-buffer-usage.h"
#include "src/graphics/display/lib/api-types/cpp/image-metadata.h"
#include "src/graphics/display/lib/api-types/cpp/mode-id.h"

namespace display::testing {

MockDisplayEngineForOutOfTree::MockDisplayEngineForOutOfTree() = default;

MockDisplayEngineForOutOfTree::~MockDisplayEngineForOutOfTree() {
  ZX_ASSERT_MSG(check_all_calls_replayed_called_, "CheckAllCallsReplayed() not called on a mock");
}

void MockDisplayEngineForOutOfTree::ExpectCheckConfiguration(CheckConfigurationChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back({.check_configuration_checker = std::move(checker)});
}

void MockDisplayEngineForOutOfTree::ExpectApplyConfiguration(ApplyConfigurationChecker checker) {
  std::lock_guard<std::mutex> lock(mutex_);
  expectations_.push_back({.apply_configuration_checker = std::move(checker)});
}

void MockDisplayEngineForOutOfTree::CheckAllCallsReplayed() {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(expectations_.empty(), "%zu expected calls were not received",
                expectations_.size());
  check_all_calls_replayed_called_ = true;
}

EngineInfo MockDisplayEngineForOutOfTree::CompleteCoordinatorConnection() {
  return display::EngineInfo{{}};
}

zx::result<> MockDisplayEngineForOutOfTree::ImportBufferCollection(
    display::DriverBufferCollectionId buffer_collection_id,
    fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> buffer_collection_token) {
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

zx::result<> MockDisplayEngineForOutOfTree::ReleaseBufferCollection(
    display::DriverBufferCollectionId buffer_collection_id) {
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

zx::result<display::DriverImageId> MockDisplayEngineForOutOfTree::ImportImage(
    const display::ImageMetadata& image_metadata,
    display::DriverBufferCollectionId buffer_collection_id, uint32_t buffer_index) {
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

zx::result<display::DriverCaptureImageId> MockDisplayEngineForOutOfTree::ImportImageForCapture(
    display::DriverBufferCollectionId buffer_collection_id, uint32_t buffer_index) {
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

void MockDisplayEngineForOutOfTree::ReleaseImage(display::DriverImageId driver_image_id) {}

display::ConfigCheckResult MockDisplayEngineForOutOfTree::CheckConfiguration(
    display::DisplayId display_id, display::ModeId display_mode_id,
    cpp20::span<const display::DriverLayer> layers) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(!expectations_.empty(), "All expected calls were already received");
  Expectation& call_expectation = expectations_.front();
  expectations_.erase(expectations_.begin());

  ZX_ASSERT_MSG(call_expectation.check_configuration_checker != nullptr,
                "Received call type does not match expected call type");
  return call_expectation.check_configuration_checker(display_id, display_mode_id, layers);
}

void MockDisplayEngineForOutOfTree::ApplyConfiguration(
    display::DisplayId display_id, display::ModeId display_mode_id,
    cpp20::span<const display::DriverLayer> layers,
    display::DriverConfigStamp driver_config_stamp) {
  std::lock_guard<std::mutex> lock(mutex_);
  ZX_ASSERT_MSG(!expectations_.empty(), "All expected calls were already received");
  Expectation& call_expectation = expectations_.front();
  expectations_.erase(expectations_.begin());

  ZX_ASSERT_MSG(call_expectation.apply_configuration_checker != nullptr,
                "Received call type does not match expected call type");
  call_expectation.apply_configuration_checker(display_id, display_mode_id, layers,
                                               driver_config_stamp);
}

zx::result<> MockDisplayEngineForOutOfTree::SetBufferCollectionConstraints(
    const display::ImageBufferUsage& image_buffer_usage,
    display::DriverBufferCollectionId buffer_collection_id) {
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

}  // namespace display::testing
