// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/layer.h"

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fidl/fuchsia.math/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/fit/defer.h>
#include <lib/zx/result.h>

#include <fbl/auto_lock.h>
#include <fbl/intrusive_single_list.h>
#include <fbl/ref_ptr.h>
#include <gtest/gtest.h>

#include "src/graphics/display/drivers/coordinator/fence.h"
#include "src/graphics/display/drivers/coordinator/image.h"
#include "src/graphics/display/drivers/coordinator/testing/base.h"
#include "src/graphics/display/drivers/fake/fake-display.h"
#include "src/graphics/display/lib/api-types/cpp/driver-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-layer-id.h"
#include "src/graphics/display/lib/api-types/cpp/event-id.h"
#include "src/lib/testing/predicates/status.h"

namespace fhdt = fuchsia_hardware_display_types;

namespace display_coordinator {

class LayerTest : public TestBase {
 public:
  void SetUp() override {
    TestBase::SetUp();
    fences_ = std::make_unique<FenceCollection>(dispatcher(), [](FenceReference*) {});
  }

  fbl::RefPtr<Image> CreateReadyImage() {
    zx::result<display::DriverImageId> import_result =
        display()->ImportVmoImageForTesting(zx::vmo(0), 0);
    EXPECT_OK(import_result);
    EXPECT_NE(import_result.value(), display::kInvalidDriverImageId);

    static constexpr display::ImageMetadata image_metadata({
        .width = kDisplayWidth,
        .height = kDisplayHeight,
        .tiling_type = display::ImageTilingType::kLinear,
    });
    fbl::RefPtr<Image> image = fbl::AdoptRef(
        new Image(controller(), image_metadata, import_result.value(), nullptr, ClientId(1)));
    image->id = next_image_id_++;
    image->Acquire();
    return image;
  }

  static void MakeLayerCurrent(Layer& layer, fbl::DoublyLinkedList<LayerNode*>& current_layers) {
    current_layers.push_front(&layer.current_node_);
  }

 protected:
  static constexpr uint32_t kDisplayWidth = 1024;
  static constexpr uint32_t kDisplayHeight = 600;

  std::unique_ptr<FenceCollection> fences_;
  display::ImageId next_image_id_ = display::ImageId(1);
};

TEST_F(LayerTest, PrimaryBasic) {
  Layer layer(display::DriverLayerId(1));
  fhdt::wire::ImageMetadata image_metadata = {
      .dimensions = {.width = kDisplayWidth, .height = kDisplayHeight},
      .tiling_type = fhdt::wire::kImageTilingTypeLinear};
  fuchsia_math::wire::RectU display_area = {.width = kDisplayWidth, .height = kDisplayHeight};
  layer.SetPrimaryConfig(image_metadata);
  layer.SetPrimaryPosition(fhdt::wire::CoordinateTransformation::kIdentity, display_area,
                           display_area);
  layer.SetPrimaryAlpha(fhdt::wire::AlphaMode::kDisable, 0);
  auto image = CreateReadyImage();
  layer.SetImage(image, display::kInvalidEventId);
  layer.ApplyChanges({.h_addressable = kDisplayWidth, .v_addressable = kDisplayHeight});
}

TEST_F(LayerTest, CleanUpImage) {
  Layer layer(display::DriverLayerId(1));
  fhdt::wire::ImageMetadata image_metadata = {
      .dimensions = {.width = kDisplayWidth, .height = kDisplayHeight},
      .tiling_type = fhdt::wire::kImageTilingTypeLinear};
  fuchsia_math::wire::RectU display_area = {.width = kDisplayWidth, .height = kDisplayHeight};
  layer.SetPrimaryConfig(image_metadata);
  layer.SetPrimaryPosition(fhdt::wire::CoordinateTransformation::kIdentity, display_area,
                           display_area);
  layer.SetPrimaryAlpha(fhdt::wire::AlphaMode::kDisable, 0);

  auto displayed_image = CreateReadyImage();
  layer.SetImage(displayed_image, display::kInvalidEventId);
  layer.ApplyChanges({.h_addressable = kDisplayWidth, .v_addressable = kDisplayHeight});
  ASSERT_TRUE(layer.ResolvePendingImage(fences_.get(), display::ConfigStamp(1)));

  zx::event event;
  ASSERT_OK(zx::event::create(0, &event));
  constexpr display::EventId kWaitFenceId(1);
  fences_->ImportEvent(std::move(event), kWaitFenceId);
  auto fence_release = fit::defer([this, kWaitFenceId] { fences_->ReleaseEvent(kWaitFenceId); });

  auto waiting_image = CreateReadyImage();
  layer.SetImage(waiting_image, kWaitFenceId);
  ASSERT_TRUE(layer.ResolvePendingImage(fences_.get(), display::ConfigStamp(2)));

  auto pending_image = CreateReadyImage();
  layer.SetImage(pending_image, display::kInvalidEventId);

  ASSERT_TRUE(layer.ActivateLatestReadyImage());

  EXPECT_TRUE(layer.current_image());
  // pending / waiting images are still busy.
  EXPECT_FALSE(pending_image->Acquire());
  EXPECT_FALSE(waiting_image->Acquire());

  // Nothing should happen if image doesn't match.
  auto not_matching_image = CreateReadyImage();
  EXPECT_FALSE(layer.CleanUpImage(*not_matching_image));
  EXPECT_TRUE(layer.current_image());
  EXPECT_FALSE(pending_image->Acquire());
  EXPECT_FALSE(waiting_image->Acquire());

  // Test cleaning up a waiting image.
  EXPECT_FALSE(layer.CleanUpImage(*waiting_image));
  EXPECT_TRUE(layer.current_image());
  EXPECT_FALSE(pending_image->Acquire());
  // waiting_image should be released.
  EXPECT_TRUE(waiting_image->Acquire());

  // Test cleaning up a pending image.
  EXPECT_FALSE(layer.CleanUpImage(*pending_image));
  EXPECT_TRUE(layer.current_image());
  // pending_image should be released.
  EXPECT_TRUE(pending_image->Acquire());

  // Test cleaning up the displayed image.
  // layer is not labeled current, so it doesn't change the current config.
  EXPECT_FALSE(layer.CleanUpImage(*displayed_image));
  EXPECT_FALSE(layer.current_image());

  // Teardown. Images must be unused (retired) when destroyed.
  displayed_image->EarlyRetire();
  not_matching_image->EarlyRetire();
  waiting_image->EarlyRetire();
  pending_image->EarlyRetire();

  // Stop the wait before tearing down the loop.
  auto fence_reference = fences_->GetFence(kWaitFenceId);
  ASSERT_TRUE(fence_reference);
  fence_reference->ResetReadyWait();
}

TEST_F(LayerTest, CleanUpImage_CheckConfigChange) {
  fbl::DoublyLinkedList<LayerNode*> current_layers;

  Layer layer(display::DriverLayerId(1));
  fhdt::wire::ImageMetadata image_metadata = {
      .dimensions = {.width = kDisplayWidth, .height = kDisplayHeight},
      .tiling_type = fhdt::wire::kImageTilingTypeLinear};
  fuchsia_math::wire::RectU display_area = {.width = kDisplayWidth, .height = kDisplayHeight};
  layer.SetPrimaryConfig(image_metadata);
  layer.SetPrimaryPosition(fhdt::wire::CoordinateTransformation::kIdentity, display_area,
                           display_area);
  layer.SetPrimaryAlpha(fhdt::wire::AlphaMode::kDisable, 0);

  // Clean up images, which doesn't change the current config.
  {
    auto image = CreateReadyImage();
    layer.SetImage(image, display::kInvalidEventId);
    layer.ApplyChanges({.h_addressable = kDisplayWidth, .v_addressable = kDisplayHeight});
    ASSERT_TRUE(layer.ResolvePendingImage(fences_.get(), display::ConfigStamp(1)));
    ASSERT_TRUE(layer.ActivateLatestReadyImage());

    EXPECT_TRUE(layer.current_image());
    // layer is not labeled current, so image cleanup doesn't change the current
    // config.
    EXPECT_FALSE(layer.CleanUpImage(*image));
    EXPECT_FALSE(layer.current_image());

    image->EarlyRetire();
  }

  // Clean up images, which changes the current config.
  {
    MakeLayerCurrent(layer, current_layers);

    auto image = CreateReadyImage();
    layer.SetImage(image, display::kInvalidEventId);
    layer.ApplyChanges({.h_addressable = kDisplayWidth, .v_addressable = kDisplayHeight});
    ASSERT_TRUE(layer.ResolvePendingImage(fences_.get(), display::ConfigStamp(2)));
    ASSERT_TRUE(layer.ActivateLatestReadyImage());

    EXPECT_TRUE(layer.current_image());
    // layer is labeled current, so image cleanup will change the current config.
    EXPECT_TRUE(layer.CleanUpImage(*image));
    EXPECT_FALSE(layer.current_image());

    image->EarlyRetire();

    current_layers.clear();
  }
}

TEST_F(LayerTest, CleanUpAllImages) {
  Layer layer(display::DriverLayerId(1));
  fhdt::wire::ImageMetadata image_metadata = {
      .dimensions = {.width = kDisplayWidth, .height = kDisplayHeight},
      .tiling_type = fhdt::wire::kImageTilingTypeLinear};
  fuchsia_math::wire::RectU display_area = {.width = kDisplayWidth, .height = kDisplayHeight};
  layer.SetPrimaryConfig(image_metadata);
  layer.SetPrimaryPosition(fhdt::wire::CoordinateTransformation::kIdentity, display_area,
                           display_area);
  layer.SetPrimaryAlpha(fhdt::wire::AlphaMode::kDisable, 0);

  auto displayed_image = CreateReadyImage();
  layer.SetImage(displayed_image, display::kInvalidEventId);
  layer.ApplyChanges({.h_addressable = kDisplayWidth, .v_addressable = kDisplayHeight});
  ASSERT_TRUE(layer.ResolvePendingImage(fences_.get(), display::ConfigStamp(1)));

  zx::event event;
  ASSERT_OK(zx::event::create(0, &event));
  constexpr display::EventId kWaitFenceId(1);
  fences_->ImportEvent(std::move(event), kWaitFenceId);
  auto fence_release = fit::defer([this, kWaitFenceId] { fences_->ReleaseEvent(kWaitFenceId); });

  auto waiting_image = CreateReadyImage();
  layer.SetImage(waiting_image, kWaitFenceId);
  ASSERT_TRUE(layer.ResolvePendingImage(fences_.get(), display::ConfigStamp(2)));

  auto pending_image = CreateReadyImage();
  layer.SetImage(pending_image, display::kInvalidEventId);

  ASSERT_TRUE(layer.ActivateLatestReadyImage());

  // layer is not labeled current, so it doesn't change the current config.
  EXPECT_FALSE(layer.CleanUpAllImages());
  EXPECT_FALSE(layer.current_image());
  // pending_image should be released.
  EXPECT_TRUE(pending_image->Acquire());
  // waiting_image should be released.
  EXPECT_TRUE(waiting_image->Acquire());

  // Teardown. Images must be unused (retired) when destroyed.
  displayed_image->EarlyRetire();
  waiting_image->EarlyRetire();
  pending_image->EarlyRetire();

  // Stop the wait before tearing down the loop.
  auto fence_reference = fences_->GetFence(kWaitFenceId);
  ASSERT_TRUE(fence_reference);
  fence_reference->ResetReadyWait();
}

TEST_F(LayerTest, CleanUpAllImages_CheckConfigChange) {
  fbl::DoublyLinkedList<LayerNode*> current_layers;

  Layer layer(display::DriverLayerId(1));
  fhdt::wire::ImageMetadata image_config = {
      .dimensions = {.width = kDisplayWidth, .height = kDisplayHeight},
      .tiling_type = fhdt::wire::kImageTilingTypeLinear};
  fuchsia_math::wire::RectU display_area = {.width = kDisplayWidth, .height = kDisplayHeight};
  layer.SetPrimaryConfig(image_config);
  layer.SetPrimaryPosition(fhdt::wire::CoordinateTransformation::kIdentity, display_area,
                           display_area);
  layer.SetPrimaryAlpha(fhdt::wire::AlphaMode::kDisable, 0);

  // Clean up all images, which doesn't change the current config.
  {
    auto image = CreateReadyImage();
    layer.SetImage(image, display::kInvalidEventId);
    layer.ApplyChanges({.h_addressable = kDisplayWidth, .v_addressable = kDisplayHeight});
    ASSERT_TRUE(layer.ResolvePendingImage(fences_.get(), display::ConfigStamp(1)));
    ASSERT_TRUE(layer.ActivateLatestReadyImage());

    EXPECT_TRUE(layer.current_image());
    // layer is not labeled current, so image cleanup doesn't change the current
    // config.
    EXPECT_FALSE(layer.CleanUpAllImages());
    EXPECT_FALSE(layer.current_image());

    image->EarlyRetire();
  }

  // Clean up all images, which changes the current config.
  {
    MakeLayerCurrent(layer, current_layers);

    auto image = CreateReadyImage();
    layer.SetImage(image, display::kInvalidEventId);
    layer.ApplyChanges({.h_addressable = kDisplayWidth, .v_addressable = kDisplayHeight});
    ASSERT_TRUE(layer.ResolvePendingImage(fences_.get(), display::ConfigStamp(2)));
    ASSERT_TRUE(layer.ActivateLatestReadyImage());

    EXPECT_TRUE(layer.current_image());
    // layer is labeled current, so image cleanup will change the current config.
    EXPECT_TRUE(layer.CleanUpAllImages());
    EXPECT_FALSE(layer.current_image());

    image->EarlyRetire();

    current_layers.clear();
  }
}

}  // namespace display_coordinator
