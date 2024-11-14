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

constexpr size_t kDisplayCount = 1;
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

  std::array<std::array<layer_t, kMaxLayerCount>, kDisplayCount> layer_ = {};

  std::array<display_config_t, kDisplayCount> configs_ = {};

  std::array<client_composition_opcode_t, kMaxLayerCount * kDisplayCount> results_ = {};

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

  for (size_t i = 0; i < kDisplayCount; i++) {
    configs_[i].display_id = i + 1;
    configs_[i].layer_list = layer_[i].data();
    configs_[i].layer_count = 1;
  }

  // Call SetupPrimaryDisplayForTesting() so that we can set up the display
  // devices without any dependency on proper driver binding.
  display_engine_->SetupPrimaryDisplayForTesting(kDisplayWidthPx, kDisplayHeightPx,
                                                 kDisplayRefreshRateHz);
}

void GoldfishDisplayEngineTest::TearDown() { allocator_binding_->Unbind(); }

TEST_F(GoldfishDisplayEngineTest, CheckConfigNoDisplay) {
  // Test No display
  size_t client_composition_opcodes_actual = 0;
  config_check_result_t res = display_engine_->DisplayEngineCheckConfiguration(
      configs_.data(), 0, results_.data(), results_.size(), &client_composition_opcodes_actual);
  EXPECT_OK(res);
}

TEST_F(GoldfishDisplayEngineTest, CheckConfigMultiLayer) {
  // ensure we fail correctly if layers more than 1
  for (size_t i = 0; i < kDisplayCount; i++) {
    configs_[i].layer_count = kMaxLayerCount;
  }

  size_t actual_result_size = 0;
  config_check_result_t res = display_engine_->DisplayEngineCheckConfiguration(
      configs_.data(), kDisplayCount, results_.data(), results_.size(), &actual_result_size);
  EXPECT_OK(res);
  EXPECT_EQ(actual_result_size, kDisplayCount * kMaxLayerCount);
  int result_cfg_offset = 0;
  for (size_t j = 0; j < kDisplayCount; j++) {
    EXPECT_EQ(CLIENT_COMPOSITION_OPCODE_MERGE_BASE,
              results_[result_cfg_offset] & CLIENT_COMPOSITION_OPCODE_MERGE_BASE);
    for (unsigned i = 1; i < kMaxLayerCount; i++) {
      EXPECT_EQ(CLIENT_COMPOSITION_OPCODE_MERGE_SRC, results_[result_cfg_offset + i]);
    }
    result_cfg_offset += kMaxLayerCount;
  }
}

TEST_F(GoldfishDisplayEngineTest, CheckConfigLayerColor) {
  constexpr int kNumLayersPerDisplay = 1;
  // First create layer for each device
  static constexpr rect_u_t kDisplayArea = {
      .x = 0,
      .y = 0,
      .width = 1024,
      .height = 768,
  };
  for (size_t i = 0; i < kDisplayCount; i++) {
    layer_[i][0].image_handle = INVALID_DISPLAY_ID;
    layer_[i][0].image_metadata = {
        .width = 0, .height = 0, .tiling_type = IMAGE_TILING_TYPE_LINEAR};
    layer_[i][0].display_destination = kDisplayArea;
    layer_[i][0].image_source = {.x = 0, .y = 0, .width = 0, .height = 0};
    layer_[i][0].alpha_mode = ALPHA_DISABLE;
    layer_[i][0].image_source_transformation = COORDINATE_TRANSFORMATION_IDENTITY;
  }

  size_t actual_result_size = 0;
  config_check_result_t res = display_engine_->DisplayEngineCheckConfiguration(
      configs_.data(), kDisplayCount, results_.data(), results_.size(), &actual_result_size);
  EXPECT_OK(res);
  EXPECT_EQ(actual_result_size, kDisplayCount * kNumLayersPerDisplay);
  for (size_t i = 0; i < kDisplayCount; i++) {
    EXPECT_EQ(CLIENT_COMPOSITION_OPCODE_USE_IMAGE,
              results_[i] & CLIENT_COMPOSITION_OPCODE_USE_IMAGE);
  }
}

TEST_F(GoldfishDisplayEngineTest, CheckConfigLayerPrimary) {
  constexpr int kNumLayersPerDisplay = 1;
  // First create layer for each device
  static constexpr rect_u_t kDisplayArea = {
      .x = 0,
      .y = 0,
      .width = 1024,
      .height = 768,
  };
  for (size_t i = 0; i < kDisplayCount; i++) {
    layer_[i][0].display_destination = kDisplayArea;
    layer_[i][0].image_source = kDisplayArea;
    layer_[i][0].image_metadata.width = 1024;
    layer_[i][0].image_metadata.height = 768;
    layer_[i][0].alpha_mode = ALPHA_DISABLE;
    layer_[i][0].image_source_transformation = COORDINATE_TRANSFORMATION_IDENTITY;
  }

  size_t actual_result_size = 0;
  config_check_result_t res = display_engine_->DisplayEngineCheckConfiguration(
      configs_.data(), kDisplayCount, results_.data(), results_.size(), &actual_result_size);
  EXPECT_OK(res);
  EXPECT_EQ(actual_result_size, kDisplayCount * kNumLayersPerDisplay);
  for (size_t i = 0; i < kDisplayCount; i++) {
    EXPECT_EQ(0u, results_[i]);
  }
}

TEST_F(GoldfishDisplayEngineTest, CheckConfigLayerDestFrame) {
  constexpr int kNumLayersPerDisplay = 1;
  // First create layer for each device
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
  for (size_t i = 0; i < kDisplayCount; i++) {
    layer_[i][0].display_destination = kDisplayDestination;
    layer_[i][0].image_source = kImageSource;
    layer_[i][0].image_metadata.width = 1024;
    layer_[i][0].image_metadata.height = 768;
  }

  size_t actual_result_size = 0;
  config_check_result_t res = display_engine_->DisplayEngineCheckConfiguration(
      configs_.data(), kDisplayCount, results_.data(), results_.size(), &actual_result_size);
  EXPECT_OK(res);
  EXPECT_EQ(actual_result_size, kDisplayCount * kNumLayersPerDisplay);
  for (size_t i = 0; i < kDisplayCount; i++) {
    EXPECT_EQ(CLIENT_COMPOSITION_OPCODE_FRAME_SCALE, results_[i]);
  }
}

TEST_F(GoldfishDisplayEngineTest, CheckConfigLayerSrcFrame) {
  constexpr int kNumLayersPerDisplay = 1;
  // First create layer for each device
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
  for (size_t i = 0; i < kDisplayCount; i++) {
    layer_[i][0].display_destination = kDisplayArea;
    layer_[i][0].image_source = kImageSource;
    layer_[i][0].image_metadata.width = 1024;
    layer_[i][0].image_metadata.height = 768;
  }

  size_t actual_result_size = 0;
  config_check_result_t res = display_engine_->DisplayEngineCheckConfiguration(
      configs_.data(), kDisplayCount, results_.data(), results_.size(), &actual_result_size);
  EXPECT_OK(res);
  EXPECT_EQ(actual_result_size, kDisplayCount * kNumLayersPerDisplay);
  for (size_t i = 0; i < kDisplayCount; i++) {
    EXPECT_EQ(CLIENT_COMPOSITION_OPCODE_SRC_FRAME, results_[i]);
  }
}

TEST_F(GoldfishDisplayEngineTest, CheckConfigLayerAlpha) {
  constexpr int kNumLayersPerDisplay = 1;
  // First create layer for each device
  static constexpr rect_u_t kDisplayArea = {
      .x = 0,
      .y = 0,
      .width = 1024,
      .height = 768,
  };
  for (size_t i = 0; i < kDisplayCount; i++) {
    layer_[i][0].display_destination = kDisplayArea;
    layer_[i][0].image_source = kDisplayArea;
    layer_[i][0].image_metadata.width = 1024;
    layer_[i][0].image_metadata.height = 768;
    layer_[i][0].alpha_mode = ALPHA_HW_MULTIPLY;
  }

  size_t actual_result_size = 0;
  config_check_result_t res = display_engine_->DisplayEngineCheckConfiguration(
      configs_.data(), kDisplayCount, results_.data(), results_.size(), &actual_result_size);
  EXPECT_OK(res);
  EXPECT_EQ(actual_result_size, kDisplayCount * kNumLayersPerDisplay);
  for (size_t i = 0; i < kDisplayCount; i++) {
    EXPECT_EQ(CLIENT_COMPOSITION_OPCODE_ALPHA, results_[i]);
  }
}

TEST_F(GoldfishDisplayEngineTest, CheckConfigLayerTransform) {
  constexpr int kNumLayersPerDisplay = 1;
  // First create layer for each device
  static constexpr rect_u_t kDisplayArea = {
      .x = 0,
      .y = 0,
      .width = 1024,
      .height = 768,
  };
  for (size_t i = 0; i < kDisplayCount; i++) {
    layer_[i][0].display_destination = kDisplayArea;
    layer_[i][0].image_source = kDisplayArea;
    layer_[i][0].image_metadata.width = 1024;
    layer_[i][0].image_metadata.height = 768;
    layer_[i][0].image_source_transformation = COORDINATE_TRANSFORMATION_REFLECT_X;
  }

  size_t actual_result_size = 0;
  config_check_result_t res = display_engine_->DisplayEngineCheckConfiguration(
      configs_.data(), kDisplayCount, results_.data(), results_.size(), &actual_result_size);
  EXPECT_OK(res);
  EXPECT_EQ(actual_result_size, kDisplayCount * kNumLayersPerDisplay);
  for (size_t i = 0; i < kDisplayCount; i++) {
    EXPECT_EQ(CLIENT_COMPOSITION_OPCODE_TRANSFORM, results_[i]);
  }
}

TEST_F(GoldfishDisplayEngineTest, CheckConfigLayerColorCoversion) {
  constexpr int kNumLayersPerDisplay = 1;
  // First create layer for each device
  static constexpr rect_u_t kDisplayArea = {
      .x = 0,
      .y = 0,
      .width = 1024,
      .height = 768,
  };
  for (size_t i = 0; i < kDisplayCount; i++) {
    layer_[i][0].display_destination = kDisplayArea;
    layer_[i][0].image_source = kDisplayArea;
    layer_[i][0].image_metadata.width = 1024;
    layer_[i][0].image_metadata.height = 768;
    configs_[i].cc_flags = COLOR_CONVERSION_POSTOFFSET;
  }

  size_t actual_result_size = 0;
  config_check_result_t res = display_engine_->DisplayEngineCheckConfiguration(
      configs_.data(), kDisplayCount, results_.data(), results_.size(), &actual_result_size);
  EXPECT_OK(res);
  EXPECT_EQ(actual_result_size, kDisplayCount * kNumLayersPerDisplay);
  for (size_t i = 0; i < kDisplayCount; i++) {
    // TODO(payamm): For now, driver will pretend it supports color conversion.
    // It should return CLIENT_COMPOSITION_OPCODE_COLOR_CONVERSION instead.
    EXPECT_EQ(0u, results_[i]);
  }
}

TEST_F(GoldfishDisplayEngineTest, CheckConfigAllFeatures) {
  constexpr int kNumLayersPerDisplay = 1;
  // First create layer for each device
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
  for (size_t i = 0; i < kDisplayCount; i++) {
    layer_[i][0].display_destination = kDisplayDestination;
    layer_[i][0].image_source = kImageSource;
    layer_[i][0].image_metadata.width = 1024;
    layer_[i][0].image_metadata.height = 768;
    layer_[i][0].alpha_mode = ALPHA_HW_MULTIPLY;
    layer_[i][0].image_source_transformation = COORDINATE_TRANSFORMATION_ROTATE_CCW_180;
    configs_[i].cc_flags = COLOR_CONVERSION_POSTOFFSET;
  }

  size_t actual_result_size = 0;
  config_check_result_t res = display_engine_->DisplayEngineCheckConfiguration(
      configs_.data(), kDisplayCount, results_.data(), results_.size(), &actual_result_size);
  EXPECT_OK(res);
  EXPECT_EQ(actual_result_size, kDisplayCount * kNumLayersPerDisplay);
  for (size_t i = 0; i < kDisplayCount; i++) {
    // TODO(https://fxbug.dev/42080897): Driver will pretend it supports color conversion
    // for now. Instead this should contain
    // CLIENT_COMPOSITION_OPCODE_COLOR_CONVERSION bit.
    EXPECT_EQ(CLIENT_COMPOSITION_OPCODE_FRAME_SCALE | CLIENT_COMPOSITION_OPCODE_SRC_FRAME |
                  CLIENT_COMPOSITION_OPCODE_ALPHA | CLIENT_COMPOSITION_OPCODE_TRANSFORM,
              results_[i]);
  }
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
