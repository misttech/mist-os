// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/intel-i915/pipe.h"

#include <lib/mmio-ptr/fake.h>
#include <lib/mmio/mmio.h>
#include <lib/sysmem-version/sysmem-version.h>

#include <memory>
#include <vector>

#include <fake-mmio-reg/fake-mmio-reg.h>
#include <gtest/gtest.h>

#include "src/graphics/display/drivers/intel-i915/hardware-common.h"
#include "src/graphics/display/drivers/intel-i915/registers-pipe.h"
#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"

namespace i915 {

class PipeTest : public ::testing::Test {
 public:
  PipeTest() = default;

  void SetUp() override { mmio_buffer_.emplace(reg_region_.GetMmioBuffer()); }

  void TearDown() override {}

 protected:
  constexpr static uint32_t kMinimumRegCount = 0xd0000 / sizeof(uint32_t);
  ddk_fake::FakeMmioRegRegion reg_region_{sizeof(uint32_t), kMinimumRegCount};
  std::optional<fdf::MmioBuffer> mmio_buffer_;
};

namespace {

class TestGttRegionImpl : public GttRegion {
 public:
  explicit TestGttRegionImpl(uint64_t handle) : handle_(handle) {}

  uint64_t bytes_per_row() const override { return 64; }
  uint64_t base() const override { return handle_ + 0xf0000000; }

 private:
  uint64_t handle_ = 0;
};

std::map<uint64_t, TestGttRegionImpl> region_map;

PixelFormatAndModifier GetPixelFormat(const image_t* image) {
  return PixelFormatAndModifier(
      fuchsia_images2::PixelFormat::kBgra32,
      /*pixel_format_modifier_param=*/fuchsia_images2::kFormatModifierLinear);
}

const GttRegion& GetGttImageHandle(const image_t* image, uint32_t rotation) {
  auto it = region_map.find(image->handle);
  if (it != region_map.end()) {
    return it->second;
  }
  return region_map.try_emplace(image->handle, image->handle).first->second;
}

layer_t CreatePrimaryLayerConfig(uint64_t handle, uint32_t z_index = 1u) {
  uint32_t kWidth = 1024u;
  uint32_t kHeight = 768u;

  layer_t layer;
  layer.type = LAYER_TYPE_PRIMARY;
  layer.z_index = z_index;
  layer.cfg.primary = {
      .image =
          {
              .width = kWidth,
              .height = kHeight,
              .type = IMAGE_TYPE_SIMPLE,
              .handle = handle,
          },
      .alpha_mode = ALPHA_DISABLE,
      .transform_mode = FRAME_TRANSFORM_IDENTITY,
      .src_frame = {0, 0, kWidth, kHeight},
      .dest_frame = {0, 0, kWidth, kHeight},
  };
  return layer;
}

}  // namespace

TEST_F(PipeTest, TiedTranscoderId) {
  PipeSkylake pipe_a(&mmio_buffer_.value(), PipeId::PIPE_A, {});
  EXPECT_EQ(TranscoderId::TRANSCODER_A, pipe_a.tied_transcoder_id());

  PipeSkylake pipe_b(&mmio_buffer_.value(), PipeId::PIPE_B, {});
  EXPECT_EQ(TranscoderId::TRANSCODER_B, pipe_b.tied_transcoder_id());

  PipeSkylake pipe_c(&mmio_buffer_.value(), PipeId::PIPE_C, {});
  EXPECT_EQ(TranscoderId::TRANSCODER_C, pipe_c.tied_transcoder_id());

  // TODO(fxbug.dev/109278): Add a test for transcoder D, when we support it.
}

// Verifies that GetVsyncConfigStamp() could return the correct config stamp
// given different image handles from device registers.
TEST_F(PipeTest, GetVsyncConfigStamp) {
  PipeSkylake pipe(&*mmio_buffer_, PipeId::PIPE_A, {});

  uint64_t kImageHandle1 = 0x1111u;
  uint64_t kImageHandle2 = 0x2222u;
  uint64_t kImageHandle3 = 0x3333u;
  layer_t layer_1 = CreatePrimaryLayerConfig(kImageHandle1, 1u);
  layer_t layer_2 = CreatePrimaryLayerConfig(kImageHandle2, 1u);
  layer_t layer_3 = CreatePrimaryLayerConfig(kImageHandle3, 2u);

  // Applies configuration with only one layer (layer_1).
  layer_t* test_layers_1[] = {&layer_1};
  display_config_t config = {
      .display_id = 1u,
      .mode = {},
      .cc_flags = 0u,
      .layer_list = test_layers_1,
      .layer_count = 1,
  };
  display::ConfigStamp stamp_1{1};
  pipe.ApplyConfiguration(&config, stamp_1, GetGttImageHandle, GetPixelFormat);

  // For images that are not registered with Pipe yet, GetVsyncConfigStamp()
  // should return nullopt.
  display::ConfigStamp vsync_config_stamp_not_found = pipe.GetVsyncConfigStamp({kImageHandle2});
  EXPECT_EQ(vsync_config_stamp_not_found, display::kInvalidConfigStamp);

  // Otherwise, for a valid image handle that has occurred in a past config,
  // GetVsyncConfigStamp() should return the latest config where it occurred.
  display::ConfigStamp vsync_config_stamp_1 = pipe.GetVsyncConfigStamp({kImageHandle1});
  EXPECT_NE(vsync_config_stamp_1, display::kInvalidConfigStamp);
  EXPECT_EQ(vsync_config_stamp_1, stamp_1);

  // Applies another configuration with two layers (layer_2 replacing layer_1,
  // and a new layer layer_3).
  layer_t* test_layers_2[] = {&layer_2, &layer_3};
  display_config_t config_2 = {
      .display_id = 1u,
      .mode = {},
      .cc_flags = 0u,
      .layer_list = test_layers_2,
      .layer_count = 1,
  };
  display::ConfigStamp stamp_2{2};
  pipe.ApplyConfiguration(&config_2, stamp_2, GetGttImageHandle, GetPixelFormat);

  // It is possible that a layer update is slower than other layers, so on
  // Vsync time the device may have layers from different configurations. In
  // that case, the device should return the oldest configuration stamp, i.e.
  // stamp_1.
  display::ConfigStamp vsync_config_stamp_2 =
      pipe.GetVsyncConfigStamp({kImageHandle1, kImageHandle3});
  EXPECT_NE(vsync_config_stamp_2, display::kInvalidConfigStamp);
  EXPECT_EQ(vsync_config_stamp_2, stamp_1);

  // Now both layers are updated in another new Vsync. GetVsyncConfigStamp()
  // should return the updated stamp value.
  display::ConfigStamp vsync_config_stamp_3 =
      pipe.GetVsyncConfigStamp({kImageHandle2, kImageHandle3});
  EXPECT_NE(vsync_config_stamp_3, display::kInvalidConfigStamp);
  EXPECT_EQ(vsync_config_stamp_3, stamp_2);

  // Old image handle should be evicted from Pipe completely.
  display::ConfigStamp vsync_config_stamp_4 =
      pipe.GetVsyncConfigStamp({kImageHandle1, kImageHandle3});
  EXPECT_EQ(vsync_config_stamp_4, display::kInvalidConfigStamp);
}

}  // namespace i915
