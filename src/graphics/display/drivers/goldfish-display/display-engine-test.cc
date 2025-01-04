// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/goldfish-display/display-engine.h"

#include <fidl/fuchsia.hardware.goldfish.pipe/cpp/wire.h>
#include <fidl/fuchsia.hardware.goldfish/cpp/wire.h>
#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <fidl/fuchsia.sysmem2/cpp/wire_test_base.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/loop.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/scoped_global_logger.h>
#include <lib/fdf/cpp/dispatcher.h>

#include <array>
#include <cstdio>
#include <memory>

#include <fbl/alloc_checker.h>
#include <fbl/array.h>
#include <fbl/vector.h>
#include <gtest/gtest.h>

#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"
#include "src/lib/testing/predicates/status.h"

namespace goldfish {

namespace {

constexpr int32_t kDisplayWidthPx = 1024;
constexpr int32_t kDisplayHeightPx = 768;
constexpr int32_t kDisplayRefreshRateHz = 60;

constexpr size_t kMaxLayerCount = 3;  // This is the max size of layer array.

}  // namespace

// TODO(https://fxbug.dev/42072949): Consider creating and using a unified set of sysmem
// testing doubles instead of writing mocks for each display driver test.
class FakeAllocator : public fidl::testing::WireTestBase<fuchsia_sysmem2::Allocator> {
 public:
  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {}
};

class FakePipe : public fidl::WireServer<fuchsia_hardware_goldfish_pipe::GoldfishPipe> {};

class GoldfishDisplayEngineTest : public testing::Test {
 public:
  GoldfishDisplayEngineTest() : loop_(&kAsyncLoopConfigNeverAttachToThread) {}

  void SetUp() override;
  void TearDown() override;

 protected:
  fdf_testing::ScopedGlobalLogger logger_;

  fdf_testing::DriverRuntime driver_runtime_;
  fdf::UnownedSynchronizedDispatcher display_event_dispatcher_ =
      driver_runtime_.StartBackgroundDispatcher();

  std::array<layer_t, kMaxLayerCount> layers_ = {};
  display_config_t config_ = {};
  std::array<layer_composition_operations_t, kMaxLayerCount> results_ = {};

  std::unique_ptr<DisplayEngine> display_engine_;

  std::optional<fidl::ServerBindingRef<fuchsia_hardware_goldfish_pipe::GoldfishPipe>> binding_;
  std::optional<fidl::ServerBindingRef<fuchsia_sysmem2::Allocator>> allocator_binding_;
  async::Loop loop_;
  FakePipe* fake_pipe_;
  FakeAllocator mock_allocator_;
};

void GoldfishDisplayEngineTest::SetUp() {
  auto [control_client, control_server] =
      fidl::Endpoints<fuchsia_hardware_goldfish::ControlDevice>::Create();
  auto [pipe_client, pipe_server] =
      fidl::Endpoints<fuchsia_hardware_goldfish_pipe::GoldfishPipe>::Create();
  auto [sysmem_client, sysmem_server] = fidl::Endpoints<fuchsia_sysmem2::Allocator>::Create();
  allocator_binding_ =
      fidl::BindServer(loop_.dispatcher(), std::move(sysmem_server), &mock_allocator_);

  display_engine_ = std::make_unique<DisplayEngine>(
      std::move(control_client), std::move(pipe_client), std::move(sysmem_client),
      std::make_unique<RenderControl>(), display_event_dispatcher_->async_dispatcher());

  config_.display_id = 1;
  config_.layer_list = layers_.data();
  config_.layer_count = 1;

  // Call SetupPrimaryDisplayForTesting() so that we can set up the display
  // devices without any dependency on proper driver binding.
  display_engine_->SetupPrimaryDisplayForTesting(kDisplayWidthPx, kDisplayHeightPx,
                                                 kDisplayRefreshRateHz);
}

void GoldfishDisplayEngineTest::TearDown() { allocator_binding_->Unbind(); }

TEST_F(GoldfishDisplayEngineTest, CheckConfigMultiLayer) {
  // ensure we fail correctly if layers more than 1
  config_.layer_count = kMaxLayerCount;

  size_t actual_result_size = 0;
  config_check_result_t res = display_engine_->DisplayEngineCheckConfiguration(
      &config_, /*display_count=*/1, results_.data(), results_.size(), &actual_result_size);
  EXPECT_EQ(CONFIG_CHECK_RESULT_UNSUPPORTED_CONFIG, res);
  EXPECT_EQ(actual_result_size, kMaxLayerCount);
  EXPECT_EQ(LAYER_COMPOSITION_OPERATIONS_MERGE_BASE,
            results_[0] & LAYER_COMPOSITION_OPERATIONS_MERGE_BASE);
  for (unsigned i = 1; i < kMaxLayerCount; ++i) {
    EXPECT_EQ(LAYER_COMPOSITION_OPERATIONS_MERGE_SRC, results_[i]);
  }
}

TEST_F(GoldfishDisplayEngineTest, CheckConfigLayerColor) {
  static constexpr rect_u_t kDisplayArea = {
      .x = 0,
      .y = 0,
      .width = 1024,
      .height = 768,
  };
  layers_[0].image_handle = INVALID_DISPLAY_ID;
  layers_[0].image_metadata = {.dimensions = {.width = 0, .height = 0},
                               .tiling_type = IMAGE_TILING_TYPE_LINEAR};
  layers_[0].display_destination = kDisplayArea;
  layers_[0].image_source = {.x = 0, .y = 0, .width = 0, .height = 0};
  layers_[0].alpha_mode = ALPHA_DISABLE;
  layers_[0].image_source_transformation = COORDINATE_TRANSFORMATION_IDENTITY;

  size_t actual_result_size = 0;
  config_check_result_t res = display_engine_->DisplayEngineCheckConfiguration(
      &config_, /*display_count=*/1, results_.data(), results_.size(), &actual_result_size);
  EXPECT_EQ(CONFIG_CHECK_RESULT_OK, res);
  EXPECT_EQ(1u, actual_result_size);
  EXPECT_EQ(LAYER_COMPOSITION_OPERATIONS_USE_IMAGE,
            results_[0] & LAYER_COMPOSITION_OPERATIONS_USE_IMAGE);
}

TEST_F(GoldfishDisplayEngineTest, CheckConfigLayerPrimary) {
  static constexpr rect_u_t kDisplayArea = {
      .x = 0,
      .y = 0,
      .width = 1024,
      .height = 768,
  };
  layers_[0].display_destination = kDisplayArea;
  layers_[0].image_source = kDisplayArea;
  layers_[0].image_metadata.dimensions = {.width = 1024, .height = 768};
  layers_[0].alpha_mode = ALPHA_DISABLE;
  layers_[0].image_source_transformation = COORDINATE_TRANSFORMATION_IDENTITY;

  size_t actual_result_size = 0;
  config_check_result_t res = display_engine_->DisplayEngineCheckConfiguration(
      &config_, /*display_count=*/1, results_.data(), results_.size(), &actual_result_size);
  EXPECT_EQ(CONFIG_CHECK_RESULT_OK, res);
  EXPECT_EQ(1u, actual_result_size);
  EXPECT_EQ(0u, results_[0]);
}

TEST_F(GoldfishDisplayEngineTest, CheckConfigLayerDestFrame) {
  static constexpr rect_u_t kDisplayDestination = {
      .x = 0,
      .y = 0,
      .width = 768,
      .height = 768,
  };
  static constexpr rect_u_t kImageSource = {
      .x = 0,
      .y = 0,
      .width = 1024,
      .height = 768,
  };
  layers_[0].display_destination = kDisplayDestination;
  layers_[0].image_source = kImageSource;
  layers_[0].image_metadata.dimensions = {.width = 1024, .height = 768};

  size_t actual_result_size = 0;
  config_check_result_t res = display_engine_->DisplayEngineCheckConfiguration(
      &config_, /*display_count=*/1, results_.data(), results_.size(), &actual_result_size);
  EXPECT_EQ(CONFIG_CHECK_RESULT_UNSUPPORTED_CONFIG, res);
  EXPECT_EQ(1u, actual_result_size);
  EXPECT_EQ(LAYER_COMPOSITION_OPERATIONS_FRAME_SCALE, results_[0]);
}

TEST_F(GoldfishDisplayEngineTest, CheckConfigLayerSrcFrame) {
  static constexpr rect_u_t kDisplayArea = {
      .x = 0,
      .y = 0,
      .width = 1024,
      .height = 768,
  };
  static constexpr rect_u_t kImageSource = {
      .x = 0,
      .y = 0,
      .width = 768,
      .height = 768,
  };
  layers_[0].display_destination = kDisplayArea;
  layers_[0].image_source = kImageSource;
  layers_[0].image_metadata.dimensions = {.width = 1024, .height = 768};

  size_t actual_result_size = 0;
  config_check_result_t res = display_engine_->DisplayEngineCheckConfiguration(
      &config_, /*display_count=*/1, results_.data(), results_.size(), &actual_result_size);
  EXPECT_EQ(CONFIG_CHECK_RESULT_UNSUPPORTED_CONFIG, res);
  EXPECT_EQ(1u, actual_result_size);
  EXPECT_EQ(LAYER_COMPOSITION_OPERATIONS_SRC_FRAME, results_[0]);
}

TEST_F(GoldfishDisplayEngineTest, CheckConfigLayerAlpha) {
  static constexpr rect_u_t kDisplayArea = {
      .x = 0,
      .y = 0,
      .width = 1024,
      .height = 768,
  };
  layers_[0].display_destination = kDisplayArea;
  layers_[0].image_source = kDisplayArea;
  layers_[0].image_metadata.dimensions = {.width = 1024, .height = 768};
  layers_[0].alpha_mode = ALPHA_HW_MULTIPLY;

  size_t actual_result_size = 0;
  config_check_result_t res = display_engine_->DisplayEngineCheckConfiguration(
      &config_, /*display_count=*/1, results_.data(), results_.size(), &actual_result_size);
  EXPECT_EQ(CONFIG_CHECK_RESULT_UNSUPPORTED_CONFIG, res);
  EXPECT_EQ(1u, actual_result_size);
  EXPECT_EQ(LAYER_COMPOSITION_OPERATIONS_ALPHA, results_[0]);
}

TEST_F(GoldfishDisplayEngineTest, CheckConfigLayerTransform) {
  static constexpr rect_u_t kDisplayArea = {
      .x = 0,
      .y = 0,
      .width = 1024,
      .height = 768,
  };
  layers_[0].display_destination = kDisplayArea;
  layers_[0].image_source = kDisplayArea;
  layers_[0].image_metadata.dimensions = {.width = 1024, .height = 768};
  layers_[0].image_source_transformation = COORDINATE_TRANSFORMATION_REFLECT_X;

  size_t actual_result_size = 0;
  config_check_result_t res = display_engine_->DisplayEngineCheckConfiguration(
      &config_, /*display_count=*/1, results_.data(), results_.size(), &actual_result_size);
  EXPECT_EQ(CONFIG_CHECK_RESULT_UNSUPPORTED_CONFIG, res);
  EXPECT_EQ(1u, actual_result_size);
  EXPECT_EQ(LAYER_COMPOSITION_OPERATIONS_TRANSFORM, results_[0]);
}

TEST_F(GoldfishDisplayEngineTest, CheckConfigLayerColorCoversion) {
  static constexpr rect_u_t kDisplayArea = {
      .x = 0,
      .y = 0,
      .width = 1024,
      .height = 768,
  };
  layers_[0].display_destination = kDisplayArea;
  layers_[0].image_source = kDisplayArea;
  layers_[0].image_metadata.dimensions = {.width = 1024, .height = 768};
  config_.cc_flags = COLOR_CONVERSION_POSTOFFSET;

  size_t actual_result_size = 0;
  config_check_result_t res = display_engine_->DisplayEngineCheckConfiguration(
      &config_, /*display_count=*/1, results_.data(), results_.size(), &actual_result_size);
  EXPECT_EQ(CONFIG_CHECK_RESULT_OK, res);
  EXPECT_EQ(1u, actual_result_size);
  // TODO(payamm): For now, driver will pretend it supports color conversion.
  // It should return LAYER_COMPOSITION_OPERATIONS_COLOR_CONVERSION instead.
  EXPECT_EQ(0u, results_[0]);
}

TEST_F(GoldfishDisplayEngineTest, CheckConfigAllFeatures) {
  static constexpr rect_u_t kDisplayDestination = {
      .x = 0,
      .y = 0,
      .width = 768,
      .height = 768,
  };
  static constexpr rect_u_t kImageSource = {
      .x = 0,
      .y = 0,
      .width = 768,
      .height = 768,
  };
  layers_[0].display_destination = kDisplayDestination;
  layers_[0].image_source = kImageSource;
  layers_[0].image_metadata.dimensions = {.width = 1024, .height = 768};
  layers_[0].alpha_mode = ALPHA_HW_MULTIPLY;
  layers_[0].image_source_transformation = COORDINATE_TRANSFORMATION_ROTATE_CCW_180;
  config_.cc_flags = COLOR_CONVERSION_POSTOFFSET;

  size_t actual_result_size = 0;
  config_check_result_t res = display_engine_->DisplayEngineCheckConfiguration(
      &config_, /*display_count=*/1, results_.data(), results_.size(), &actual_result_size);
  EXPECT_EQ(CONFIG_CHECK_RESULT_UNSUPPORTED_CONFIG, res);
  EXPECT_EQ(1u, actual_result_size);
  // TODO(https://fxbug.dev/42080897): Driver will pretend it supports color conversion
  // for now. Instead this should contain
  // LAYER_COMPOSITION_OPERATIONS_COLOR_CONVERSION bit.
  EXPECT_EQ(LAYER_COMPOSITION_OPERATIONS_FRAME_SCALE | LAYER_COMPOSITION_OPERATIONS_SRC_FRAME |
                LAYER_COMPOSITION_OPERATIONS_ALPHA | LAYER_COMPOSITION_OPERATIONS_TRANSFORM,
            results_[0]);
}

TEST_F(GoldfishDisplayEngineTest, ImportBufferCollection) {
  zx::result token1_endpoints = fidl::CreateEndpoints<fuchsia_sysmem2::BufferCollectionToken>();
  ASSERT_TRUE(token1_endpoints.is_ok());
  zx::result token2_endpoints = fidl::CreateEndpoints<fuchsia_sysmem2::BufferCollectionToken>();
  ASSERT_TRUE(token2_endpoints.is_ok());

  // Test ImportBufferCollection().
  constexpr display::DriverBufferCollectionId kValidCollectionId(1);
  constexpr uint64_t kBanjoValidCollectionId =
      display::ToBanjoDriverBufferCollectionId(kValidCollectionId);
  EXPECT_OK(display_engine_->DisplayEngineImportBufferCollection(
      kBanjoValidCollectionId, token1_endpoints->client.TakeChannel()));

  // `collection_id` must be unused.
  EXPECT_EQ(display_engine_->DisplayEngineImportBufferCollection(
                kBanjoValidCollectionId, token2_endpoints->client.TakeChannel()),
            ZX_ERR_ALREADY_EXISTS);

  // Test ReleaseBufferCollection().
  constexpr display::DriverBufferCollectionId kInvalidCollectionId(2);
  constexpr uint64_t kBanjoInvalidCollectionId =
      display::ToBanjoDriverBufferCollectionId(kInvalidCollectionId);
  EXPECT_EQ(display_engine_->DisplayEngineReleaseBufferCollection(kBanjoInvalidCollectionId),
            ZX_ERR_NOT_FOUND);
  EXPECT_OK(display_engine_->DisplayEngineReleaseBufferCollection(kBanjoValidCollectionId));

  loop_.Shutdown();
}

// TODO(https://fxbug.dev/42073664): Implement a fake sysmem and a fake goldfish-pipe
// driver to test importing images using ImportImage().

}  // namespace goldfish
