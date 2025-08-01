// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_PROTOCOLS_CPP_MOCK_DISPLAY_ENGINE_FOR_OUT_OF_TREE_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_PROTOCOLS_CPP_MOCK_DISPLAY_ENGINE_FOR_OUT_OF_TREE_H_

#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <lib/fit/function.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/result.h>
#include <zircon/compiler.h>

#include <cstddef>
#include <cstdint>
#include <list>
#include <mutex>

#include "src/graphics/display/lib/api-protocols/cpp/display-engine-interface.h"
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

// Strict mock for DisplayEngineInterface implementations for out-of-tree
// display drivers.
class MockDisplayEngineForOutOfTree final : public display::DisplayEngineInterface {
 public:
  using CheckConfigurationChecker = fit::function<display::ConfigCheckResult(
      display::DisplayId display_id, display::ModeId display_mode_id,
      cpp20::span<const display::DriverLayer> layers)>;
  using ApplyConfigurationChecker = fit::function<void(
      display::DisplayId display_id, display::ModeId display_mode_id,
      cpp20::span<const display::DriverLayer> layers, display::DriverConfigStamp config_stamp)>;

  MockDisplayEngineForOutOfTree();
  MockDisplayEngineForOutOfTree(const MockDisplayEngineForOutOfTree&) = delete;
  MockDisplayEngineForOutOfTree& operator=(const MockDisplayEngineForOutOfTree&) = delete;
  ~MockDisplayEngineForOutOfTree();

  void ExpectCheckConfiguration(CheckConfigurationChecker checker);
  void ExpectApplyConfiguration(ApplyConfigurationChecker checker);

  // Must be called at least once during an instance's lifetime.
  //
  // Tests are recommended to call this in a TearDown() method, or at the end of
  // the test case implementation.
  void CheckAllCallsReplayed();

  // `display::DisplayEngineInterface`:
  EngineInfo CompleteCoordinatorConnection() override;
  zx::result<> ImportBufferCollection(
      display::DriverBufferCollectionId buffer_collection_id,
      fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> buffer_collection_token) override;
  zx::result<> ReleaseBufferCollection(
      display::DriverBufferCollectionId buffer_collection_id) override;
  zx::result<display::DriverImageId> ImportImage(
      const display::ImageMetadata& image_metadata,
      display::DriverBufferCollectionId buffer_collection_id, uint32_t buffer_index) override;
  zx::result<display::DriverCaptureImageId> ImportImageForCapture(
      display::DriverBufferCollectionId buffer_collection_id, uint32_t buffer_index) override;
  void ReleaseImage(display::DriverImageId driver_image_id) override;
  display::ConfigCheckResult CheckConfiguration(
      display::DisplayId display_id, display::ModeId display_mode_id,
      cpp20::span<const display::DriverLayer> layers) override;
  void ApplyConfiguration(display::DisplayId display_id, display::ModeId display_mode_id,
                          cpp20::span<const display::DriverLayer> layers,
                          display::DriverConfigStamp driver_config_stamp) override;
  zx::result<> SetBufferCollectionConstraints(
      const display::ImageBufferUsage& image_buffer_usage,
      display::DriverBufferCollectionId buffer_collection_id) override;

 private:
  struct Expectation {
    CheckConfigurationChecker check_configuration_checker;
    ApplyConfigurationChecker apply_configuration_checker;
  };

  std::mutex mutex_;
  std::list<Expectation> expectations_ __TA_GUARDED(mutex_);
  bool check_all_calls_replayed_called_ __TA_GUARDED(mutex_) = false;
};

}  // namespace display::testing

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_PROTOCOLS_CPP_MOCK_DISPLAY_ENGINE_FOR_OUT_OF_TREE_H_
