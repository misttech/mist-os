// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/goldfish-display/display.h"

#include <fidl/fuchsia.sysmem/cpp/wire_test_base.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/loop.h>
#include <lib/ddk/device.h>

#include <array>
#include <cstdio>
#include <memory>

#include <fbl/alloc_checker.h>
#include <fbl/array.h>
#include <fbl/vector.h>
#include <gtest/gtest.h>

#include "src/graphics/display/lib/api-types-cpp/driver-buffer-collection-id.h"
#include "src/lib/testing/predicates/status.h"

namespace goldfish {

namespace {
constexpr size_t kNumDisplays = 2;
constexpr size_t kMaxLayerCount = 3;  // This is the max size of layer array.
}  // namespace

// TODO(fxbug.dev/121924): Consider creating and using a unified set of sysmem
// testing doubles instead of writing mocks for each display driver test.
class FakeAllocator : public fidl::testing::WireTestBase<fuchsia_sysmem::Allocator> {
 public:
  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {}
};

class FakePipe : public fidl::WireServer<fuchsia_hardware_goldfish_pipe::GoldfishPipe> {};

class GoldfishDisplayTest : public testing::Test {
 public:
  GoldfishDisplayTest() : loop_(&kAsyncLoopConfigNeverAttachToThread) {}

  void SetUp() override;
  void TearDown() override;
  std::array<std::array<layer_t, kMaxLayerCount>, kNumDisplays> layer_ = {};
  std::array<layer_t*, kNumDisplays> layer_ptrs = {};

  std::array<display_config_t, kNumDisplays> configs_ = {};
  std::array<display_config_t*, kNumDisplays> configs_ptrs_ = {};

  std::array<client_composition_opcode_t, kMaxLayerCount * kNumDisplays> results_ = {};

  std::unique_ptr<Display> display_ = {};

  std::optional<fidl::ServerBindingRef<fuchsia_hardware_goldfish_pipe::GoldfishPipe>> binding_;
  std::optional<fidl::ServerBindingRef<fuchsia_sysmem::Allocator>> allocator_binding_;
  async::Loop loop_;
  FakePipe* fake_pipe_;
  FakeAllocator mock_allocator_;
};

void GoldfishDisplayTest::SetUp() {
  display_ = std::make_unique<Display>(nullptr);

  for (size_t i = 0; i < kNumDisplays; i++) {
    configs_ptrs_[i] = &configs_[i];
    layer_ptrs[i] = &layer_[i][0];
    configs_[i].display_id = i + 1;
    configs_[i].layer_list = &layer_ptrs[i];
    configs_[i].layer_count = 1;
  }

  // Need CreateDevices and RemoveDevices to ensure we can test CheckConfiguration without any
  // dependency on proper driver binding/loading
  display_->CreateDevices(kNumDisplays);

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_sysmem::Allocator>();
  ASSERT_TRUE(endpoints.is_ok());
  allocator_binding_ =
      fidl::BindServer(loop_.dispatcher(), std::move(endpoints->server), &mock_allocator_);
  display_->SetSysmemAllocatorForTesting(fidl::WireSyncClient(std::move(endpoints->client)));
}

void GoldfishDisplayTest::TearDown() {
  allocator_binding_->Unbind();
  display_->RemoveDevices();
}

TEST_F(GoldfishDisplayTest, CheckConfigNoDisplay) {
  // Test No display
  size_t layer_cfg_results_actual = 0;
  config_check_result_t res = display_->DisplayControllerImplCheckConfiguration(
      const_cast<const display_config_t**>(configs_ptrs_.data()), 0, results_.data(),
      results_.size(), &layer_cfg_results_actual);
  EXPECT_OK(res);
}

TEST_F(GoldfishDisplayTest, CheckConfigMultiLayer) {
  // ensure we fail correctly if layers more than 1
  for (size_t i = 0; i < kNumDisplays; i++) {
    configs_[i].layer_count = kMaxLayerCount;
  }

  size_t actual_result_size = 0;
  config_check_result_t res = display_->DisplayControllerImplCheckConfiguration(
      const_cast<const display_config_t**>(configs_ptrs_.data()), kNumDisplays, results_.data(),
      results_.size(), &actual_result_size);
  EXPECT_OK(res);
  EXPECT_EQ(actual_result_size, kNumDisplays * kMaxLayerCount);
  int result_cfg_offset = 0;
  for (size_t j = 0; j < kNumDisplays; j++) {
    EXPECT_EQ(CLIENT_COMPOSITION_OPCODE_MERGE_BASE,
              results_[result_cfg_offset] & CLIENT_COMPOSITION_OPCODE_MERGE_BASE);
    for (unsigned i = 1; i < kMaxLayerCount; i++) {
      EXPECT_EQ(CLIENT_COMPOSITION_OPCODE_MERGE_SRC, results_[result_cfg_offset + i]);
    }
    result_cfg_offset += kMaxLayerCount;
  }
}

TEST_F(GoldfishDisplayTest, CheckConfigLayerColor) {
  constexpr int kNumLayersPerDisplay = 1;
  // First create layer for each device
  for (size_t i = 0; i < kNumDisplays; i++) {
    configs_[i].layer_list[0]->type = LAYER_TYPE_COLOR;
  }

  size_t actual_result_size = 0;
  config_check_result_t res = display_->DisplayControllerImplCheckConfiguration(
      const_cast<const display_config_t**>(configs_ptrs_.data()), kNumDisplays, results_.data(),
      results_.size(), &actual_result_size);
  EXPECT_OK(res);
  EXPECT_EQ(actual_result_size, kNumDisplays * kNumLayersPerDisplay);
  for (size_t i = 0; i < kNumDisplays; i++) {
    EXPECT_EQ(CLIENT_COMPOSITION_OPCODE_USE_PRIMARY,
              results_[i] & CLIENT_COMPOSITION_OPCODE_USE_PRIMARY);
  }
}

TEST_F(GoldfishDisplayTest, CheckConfigLayerCursor) {
  constexpr int kNumLayersPerDisplay = 1;
  // First create layer for each device
  for (size_t i = 0; i < kNumDisplays; i++) {
    configs_[i].layer_list[0]->type = LAYER_TYPE_CURSOR;
  }

  size_t actual_result_size = 0;
  config_check_result_t res = display_->DisplayControllerImplCheckConfiguration(
      const_cast<const display_config_t**>(configs_ptrs_.data()), kNumDisplays, results_.data(),
      results_.size(), &actual_result_size);
  EXPECT_OK(res);
  EXPECT_EQ(actual_result_size, kNumDisplays * kNumLayersPerDisplay);
  for (size_t i = 0; i < kNumDisplays; i++) {
    EXPECT_EQ(CLIENT_COMPOSITION_OPCODE_USE_PRIMARY,
              results_[i] & CLIENT_COMPOSITION_OPCODE_USE_PRIMARY);
  }
}

TEST_F(GoldfishDisplayTest, CheckConfigLayerPrimary) {
  constexpr int kNumLayersPerDisplay = 1;
  // First create layer for each device
  frame_t dest_frame = {
      .x_pos = 0,
      .y_pos = 0,
      .width = 1024,
      .height = 768,
  };
  frame_t src_frame = {
      .x_pos = 0,
      .y_pos = 0,
      .width = 1024,
      .height = 768,
  };
  for (size_t i = 0; i < kNumDisplays; i++) {
    configs_[i].layer_list[0]->cfg.primary.dest_frame = dest_frame;
    configs_[i].layer_list[0]->cfg.primary.src_frame = src_frame;
    configs_[i].layer_list[0]->cfg.primary.image.width = 1024;
    configs_[i].layer_list[0]->cfg.primary.image.height = 768;
    configs_[i].layer_list[0]->cfg.primary.alpha_mode = 0;
    configs_[i].layer_list[0]->cfg.primary.transform_mode = 0;
  }

  size_t actual_result_size = 0;
  config_check_result_t res = display_->DisplayControllerImplCheckConfiguration(
      const_cast<const display_config_t**>(configs_ptrs_.data()), kNumDisplays, results_.data(),
      results_.size(), &actual_result_size);
  EXPECT_OK(res);
  EXPECT_EQ(actual_result_size, kNumDisplays * kNumLayersPerDisplay);
  for (size_t i = 0; i < kNumDisplays; i++) {
    EXPECT_EQ(0u, results_[i]);
  }
}

TEST_F(GoldfishDisplayTest, CheckConfigLayerDestFrame) {
  constexpr int kNumLayersPerDisplay = 1;
  // First create layer for each device
  frame_t dest_frame = {
      .x_pos = 0,
      .y_pos = 0,
      .width = 768,
      .height = 768,
  };
  frame_t src_frame = {
      .x_pos = 0,
      .y_pos = 0,
      .width = 1024,
      .height = 768,
  };
  for (size_t i = 0; i < kNumDisplays; i++) {
    configs_[i].layer_list[0]->cfg.primary.dest_frame = dest_frame;
    configs_[i].layer_list[0]->cfg.primary.src_frame = src_frame;
    configs_[i].layer_list[0]->cfg.primary.image.width = 1024;
    configs_[i].layer_list[0]->cfg.primary.image.height = 768;
  }

  size_t actual_result_size = 0;
  config_check_result_t res = display_->DisplayControllerImplCheckConfiguration(
      const_cast<const display_config_t**>(configs_ptrs_.data()), kNumDisplays, results_.data(),
      results_.size(), &actual_result_size);
  EXPECT_OK(res);
  EXPECT_EQ(actual_result_size, kNumDisplays * kNumLayersPerDisplay);
  for (size_t i = 0; i < kNumDisplays; i++) {
    EXPECT_EQ(CLIENT_COMPOSITION_OPCODE_FRAME_SCALE, results_[i]);
  }
}

TEST_F(GoldfishDisplayTest, CheckConfigLayerSrcFrame) {
  constexpr int kNumLayersPerDisplay = 1;
  // First create layer for each device
  frame_t dest_frame = {
      .x_pos = 0,
      .y_pos = 0,
      .width = 1024,
      .height = 768,
  };
  frame_t src_frame = {
      .x_pos = 0,
      .y_pos = 0,
      .width = 768,
      .height = 768,
  };
  for (size_t i = 0; i < kNumDisplays; i++) {
    configs_[i].layer_list[0]->cfg.primary.dest_frame = dest_frame;
    configs_[i].layer_list[0]->cfg.primary.src_frame = src_frame;
    configs_[i].layer_list[0]->cfg.primary.image.width = 1024;
    configs_[i].layer_list[0]->cfg.primary.image.height = 768;
  }

  size_t actual_result_size = 0;
  config_check_result_t res = display_->DisplayControllerImplCheckConfiguration(
      const_cast<const display_config_t**>(configs_ptrs_.data()), kNumDisplays, results_.data(),
      results_.size(), &actual_result_size);
  EXPECT_OK(res);
  EXPECT_EQ(actual_result_size, kNumDisplays * kNumLayersPerDisplay);
  for (size_t i = 0; i < kNumDisplays; i++) {
    EXPECT_EQ(CLIENT_COMPOSITION_OPCODE_SRC_FRAME, results_[i]);
  }
}

TEST_F(GoldfishDisplayTest, CheckConfigLayerAlpha) {
  constexpr int kNumLayersPerDisplay = 1;
  // First create layer for each device
  frame_t dest_frame = {
      .x_pos = 0,
      .y_pos = 0,
      .width = 1024,
      .height = 768,
  };
  frame_t src_frame = {
      .x_pos = 0,
      .y_pos = 0,
      .width = 1024,
      .height = 768,
  };
  for (size_t i = 0; i < kNumDisplays; i++) {
    configs_[i].layer_list[0]->cfg.primary.dest_frame = dest_frame;
    configs_[i].layer_list[0]->cfg.primary.src_frame = src_frame;
    configs_[i].layer_list[0]->cfg.primary.image.width = 1024;
    configs_[i].layer_list[0]->cfg.primary.image.height = 768;
    configs_[i].layer_list[0]->cfg.primary.alpha_mode = ALPHA_HW_MULTIPLY;
  }

  size_t actual_result_size = 0;
  config_check_result_t res = display_->DisplayControllerImplCheckConfiguration(
      const_cast<const display_config_t**>(configs_ptrs_.data()), kNumDisplays, results_.data(),
      results_.size(), &actual_result_size);
  EXPECT_OK(res);
  EXPECT_EQ(actual_result_size, kNumDisplays * kNumLayersPerDisplay);
  for (size_t i = 0; i < kNumDisplays; i++) {
    EXPECT_EQ(CLIENT_COMPOSITION_OPCODE_ALPHA, results_[i]);
  }
}

TEST_F(GoldfishDisplayTest, CheckConfigLayerTransform) {
  constexpr int kNumLayersPerDisplay = 1;
  // First create layer for each device
  frame_t dest_frame = {
      .x_pos = 0,
      .y_pos = 0,
      .width = 1024,
      .height = 768,
  };
  frame_t src_frame = {
      .x_pos = 0,
      .y_pos = 0,
      .width = 1024,
      .height = 768,
  };
  for (size_t i = 0; i < kNumDisplays; i++) {
    configs_[i].layer_list[0]->cfg.primary.dest_frame = dest_frame;
    configs_[i].layer_list[0]->cfg.primary.src_frame = src_frame;
    configs_[i].layer_list[0]->cfg.primary.image.width = 1024;
    configs_[i].layer_list[0]->cfg.primary.image.height = 768;
    configs_[i].layer_list[0]->cfg.primary.transform_mode = FRAME_TRANSFORM_REFLECT_X;
  }

  size_t actual_result_size = 0;
  config_check_result_t res = display_->DisplayControllerImplCheckConfiguration(
      const_cast<const display_config_t**>(configs_ptrs_.data()), kNumDisplays, results_.data(),
      results_.size(), &actual_result_size);
  EXPECT_OK(res);
  EXPECT_EQ(actual_result_size, kNumDisplays * kNumLayersPerDisplay);
  for (size_t i = 0; i < kNumDisplays; i++) {
    EXPECT_EQ(CLIENT_COMPOSITION_OPCODE_TRANSFORM, results_[i]);
  }
}

TEST_F(GoldfishDisplayTest, CheckConfigLayerColorCoversion) {
  constexpr int kNumLayersPerDisplay = 1;
  // First create layer for each device
  frame_t dest_frame = {
      .x_pos = 0,
      .y_pos = 0,
      .width = 1024,
      .height = 768,
  };
  frame_t src_frame = {
      .x_pos = 0,
      .y_pos = 0,
      .width = 1024,
      .height = 768,
  };
  for (size_t i = 0; i < kNumDisplays; i++) {
    configs_[i].layer_list[0]->cfg.primary.dest_frame = dest_frame;
    configs_[i].layer_list[0]->cfg.primary.src_frame = src_frame;
    configs_[i].layer_list[0]->cfg.primary.image.width = 1024;
    configs_[i].layer_list[0]->cfg.primary.image.height = 768;
    configs_[i].cc_flags = COLOR_CONVERSION_POSTOFFSET;
  }

  size_t actual_result_size = 0;
  config_check_result_t res = display_->DisplayControllerImplCheckConfiguration(
      const_cast<const display_config_t**>(configs_ptrs_.data()), kNumDisplays, results_.data(),
      results_.size(), &actual_result_size);
  EXPECT_OK(res);
  EXPECT_EQ(actual_result_size, kNumDisplays * kNumLayersPerDisplay);
  for (size_t i = 0; i < kNumDisplays; i++) {
    // TODO(payamm): For now, driver will pretend it supports color conversion.
    // It should return CLIENT_COMPOSITION_OPCODE_COLOR_CONVERSION instead.
    EXPECT_EQ(0u, results_[i]);
  }
}

TEST_F(GoldfishDisplayTest, CheckConfigAllFeatures) {
  constexpr int kNumLayersPerDisplay = 1;
  // First create layer for each device
  frame_t dest_frame = {
      .x_pos = 0,
      .y_pos = 0,
      .width = 768,
      .height = 768,
  };
  frame_t src_frame = {
      .x_pos = 0,
      .y_pos = 0,
      .width = 768,
      .height = 768,
  };
  for (size_t i = 0; i < kNumDisplays; i++) {
    configs_[i].layer_list[0]->cfg.primary.dest_frame = dest_frame;
    configs_[i].layer_list[0]->cfg.primary.src_frame = src_frame;
    configs_[i].layer_list[0]->cfg.primary.image.width = 1024;
    configs_[i].layer_list[0]->cfg.primary.image.height = 768;
    configs_[i].layer_list[0]->cfg.primary.alpha_mode = ALPHA_HW_MULTIPLY;
    configs_[i].layer_list[0]->cfg.primary.transform_mode = FRAME_TRANSFORM_ROT_180;
    configs_[i].cc_flags = COLOR_CONVERSION_POSTOFFSET;
  }

  size_t actual_result_size = 0;
  config_check_result_t res = display_->DisplayControllerImplCheckConfiguration(
      const_cast<const display_config_t**>(configs_ptrs_.data()), kNumDisplays, results_.data(),
      results_.size(), &actual_result_size);
  EXPECT_OK(res);
  EXPECT_EQ(actual_result_size, kNumDisplays * kNumLayersPerDisplay);
  for (size_t i = 0; i < kNumDisplays; i++) {
    // TODO(fxbug.dev/130606): Driver will pretend it supports color conversion
    // for now. Instead this should contain
    // CLIENT_COMPOSITION_OPCODE_COLOR_CONVERSION bit.
    EXPECT_EQ(CLIENT_COMPOSITION_OPCODE_FRAME_SCALE | CLIENT_COMPOSITION_OPCODE_SRC_FRAME |
                  CLIENT_COMPOSITION_OPCODE_ALPHA | CLIENT_COMPOSITION_OPCODE_TRANSFORM,
              results_[i]);
  }
}

TEST_F(GoldfishDisplayTest, ImportBufferCollection) {
  zx::result token1_endpoints = fidl::CreateEndpoints<fuchsia_sysmem::BufferCollectionToken>();
  ASSERT_TRUE(token1_endpoints.is_ok());
  zx::result token2_endpoints = fidl::CreateEndpoints<fuchsia_sysmem::BufferCollectionToken>();
  ASSERT_TRUE(token2_endpoints.is_ok());

  // Test ImportBufferCollection().
  constexpr display::DriverBufferCollectionId kValidCollectionId(1);
  constexpr uint64_t kBanjoValidCollectionId =
      display::ToBanjoDriverBufferCollectionId(kValidCollectionId);
  EXPECT_OK(display_->DisplayControllerImplImportBufferCollection(
      kBanjoValidCollectionId, token1_endpoints->client.TakeChannel()));

  // `collection_id` must be unused.
  EXPECT_EQ(display_->DisplayControllerImplImportBufferCollection(
                kBanjoValidCollectionId, token2_endpoints->client.TakeChannel()),
            ZX_ERR_ALREADY_EXISTS);

  // Test ReleaseBufferCollection().
  constexpr display::DriverBufferCollectionId kInvalidCollectionId(2);
  constexpr uint64_t kBanjoInvalidCollectionId =
      display::ToBanjoDriverBufferCollectionId(kInvalidCollectionId);
  EXPECT_EQ(display_->DisplayControllerImplReleaseBufferCollection(kBanjoInvalidCollectionId),
            ZX_ERR_NOT_FOUND);
  EXPECT_OK(display_->DisplayControllerImplReleaseBufferCollection(kBanjoValidCollectionId));

  loop_.Shutdown();
}

// TODO(fxbug.dev/122687): Implement a fake sysmem and a fake goldfish-pipe
// driver to test importing images using ImportImage().

}  // namespace goldfish
