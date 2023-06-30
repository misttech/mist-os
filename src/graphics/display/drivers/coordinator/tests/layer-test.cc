// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/layer.h"

#include <lib/async-loop/cpp/loop.h>
#include <lib/fit/defer.h>

#include <fbl/auto_lock.h>
#include <fbl/intrusive_single_list.h>
#include <gtest/gtest.h>

#include "src/graphics/display/drivers/coordinator/controller.h"
#include "src/graphics/display/drivers/coordinator/fence.h"
#include "src/graphics/display/drivers/coordinator/image.h"
#include "src/graphics/display/drivers/coordinator/tests/base.h"
#include "src/graphics/display/drivers/fake/fake-display.h"
#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types-cpp/driver-layer-id.h"
#include "src/graphics/display/lib/api-types-cpp/event-id.h"
#include "src/lib/testing/predicates/status.h"

namespace fhd = fuchsia_hardware_display;

namespace display {

class LayerTest : public TestBase {
 public:
  void SetUp() override {
    TestBase::SetUp();
    fences_ = std::make_unique<FenceCollection>(dispatcher(), [](FenceReference*) {});
  }

  fbl::RefPtr<Image> CreateReadyImage() {
    image_t dc_image = {
        .width = kDisplayWidth,
        .height = kDisplayHeight,
        .type = fhd::wire::kTypeSimple,
        .handle = 0,
    };
    EXPECT_OK(display()->ImportVmoImage(&dc_image, zx::vmo(0), 0));
    EXPECT_NE(dc_image.handle, 0u);
    auto image =
        fbl::AdoptRef(new Image(controller(), dc_image, zx::vmo(0), nullptr, /*client_id=*/1u));
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
  ImageId next_image_id_ = ImageId(1);
};

TEST_F(LayerTest, PrimaryBasic) {
  Layer layer(DriverLayerId(1));
  fhd::wire::ImageConfig image_config = {
      .width = kDisplayWidth, .height = kDisplayHeight, .type = fhd::wire::kTypeSimple};
  fhd::wire::Frame frame = {.width = kDisplayWidth, .height = kDisplayHeight};
  layer.SetPrimaryConfig(image_config);
  layer.SetPrimaryPosition(fhd::wire::Transform::kIdentity, frame, frame);
  layer.SetPrimaryAlpha(fhd::wire::AlphaMode::kDisable, 0);
  auto image = CreateReadyImage();
  layer.SetImage(image, kInvalidEventId, kInvalidEventId);
  layer.ApplyChanges({.h_addressable = kDisplayWidth, .v_addressable = kDisplayHeight});
}

TEST_F(LayerTest, CursorBasic) {
  Layer layer(DriverLayerId(1));
  layer.SetCursorConfig({});
  layer.SetCursorPosition(/*x=*/4, /*y=*/10);
  auto image = CreateReadyImage();
  layer.SetImage(image, kInvalidEventId, kInvalidEventId);
  layer.ApplyChanges({.h_addressable = kDisplayWidth, .v_addressable = kDisplayHeight});
}

TEST_F(LayerTest, CleanUpImage) {
  Layer layer(DriverLayerId(1));
  fhd::wire::ImageConfig image_config = {
      .width = kDisplayWidth, .height = kDisplayHeight, .type = fhd::wire::kTypeSimple};
  fhd::wire::Frame frame = {.width = kDisplayWidth, .height = kDisplayHeight};
  layer.SetPrimaryConfig(image_config);
  layer.SetPrimaryPosition(fhd::wire::Transform::kIdentity, frame, frame);
  layer.SetPrimaryAlpha(fhd::wire::AlphaMode::kDisable, 0);

  auto displayed_image = CreateReadyImage();
  layer.SetImage(displayed_image, kInvalidEventId, kInvalidEventId);
  layer.ApplyChanges({.h_addressable = kDisplayWidth, .v_addressable = kDisplayHeight});
  ASSERT_TRUE(layer.ResolvePendingImage(fences_.get(), ConfigStamp(1)));

  zx::event event;
  ASSERT_OK(zx::event::create(0, &event));
  constexpr EventId kWaitFenceId(1);
  fences_->ImportEvent(std::move(event), kWaitFenceId);
  auto fence_release = fit::defer([this, kWaitFenceId] { fences_->ReleaseEvent(kWaitFenceId); });

  auto waiting_image = CreateReadyImage();
  layer.SetImage(waiting_image, kWaitFenceId, kInvalidEventId);
  ASSERT_TRUE(layer.ResolvePendingImage(fences_.get(), ConfigStamp(2)));

  auto pending_image = CreateReadyImage();
  layer.SetImage(pending_image, kInvalidEventId, kInvalidEventId);

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

  Layer layer(DriverLayerId(1));
  fhd::wire::ImageConfig image_config = {
      .width = kDisplayWidth, .height = kDisplayHeight, .type = fhd::wire::kTypeSimple};
  fhd::wire::Frame frame = {.width = kDisplayWidth, .height = kDisplayHeight};
  layer.SetPrimaryConfig(image_config);
  layer.SetPrimaryPosition(fhd::wire::Transform::kIdentity, frame, frame);
  layer.SetPrimaryAlpha(fhd::wire::AlphaMode::kDisable, 0);

  // Clean up images, which doesn't change the current config.
  {
    auto image = CreateReadyImage();
    layer.SetImage(image, kInvalidEventId, kInvalidEventId);
    layer.ApplyChanges({.h_addressable = kDisplayWidth, .v_addressable = kDisplayHeight});
    ASSERT_TRUE(layer.ResolvePendingImage(fences_.get(), ConfigStamp(1)));
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
    layer.SetImage(image, kInvalidEventId, kInvalidEventId);
    layer.ApplyChanges({.h_addressable = kDisplayWidth, .v_addressable = kDisplayHeight});
    ASSERT_TRUE(layer.ResolvePendingImage(fences_.get(), ConfigStamp(2)));
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
  Layer layer(DriverLayerId(1));
  fhd::wire::ImageConfig image_config = {
      .width = kDisplayWidth, .height = kDisplayHeight, .type = fhd::wire::kTypeSimple};
  fhd::wire::Frame frame = {.width = kDisplayWidth, .height = kDisplayHeight};
  layer.SetPrimaryConfig(image_config);
  layer.SetPrimaryPosition(fhd::wire::Transform::kIdentity, frame, frame);
  layer.SetPrimaryAlpha(fhd::wire::AlphaMode::kDisable, 0);

  auto displayed_image = CreateReadyImage();
  layer.SetImage(displayed_image, kInvalidEventId, kInvalidEventId);
  layer.ApplyChanges({.h_addressable = kDisplayWidth, .v_addressable = kDisplayHeight});
  ASSERT_TRUE(layer.ResolvePendingImage(fences_.get(), ConfigStamp(1)));

  zx::event event;
  ASSERT_OK(zx::event::create(0, &event));
  constexpr EventId kWaitFenceId(1);
  fences_->ImportEvent(std::move(event), kWaitFenceId);
  auto fence_release = fit::defer([this, kWaitFenceId] { fences_->ReleaseEvent(kWaitFenceId); });

  auto waiting_image = CreateReadyImage();
  layer.SetImage(waiting_image, kWaitFenceId, kInvalidEventId);
  ASSERT_TRUE(layer.ResolvePendingImage(fences_.get(), ConfigStamp(2)));

  auto pending_image = CreateReadyImage();
  layer.SetImage(pending_image, kInvalidEventId, kInvalidEventId);

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

  Layer layer(DriverLayerId(1));
  fhd::wire::ImageConfig image_config = {
      .width = kDisplayWidth, .height = kDisplayHeight, .type = fhd::wire::kTypeSimple};
  fhd::wire::Frame frame = {.width = kDisplayWidth, .height = kDisplayHeight};
  layer.SetPrimaryConfig(image_config);
  layer.SetPrimaryPosition(fhd::wire::Transform::kIdentity, frame, frame);
  layer.SetPrimaryAlpha(fhd::wire::AlphaMode::kDisable, 0);

  // Clean up all images, which doesn't change the current config.
  {
    auto image = CreateReadyImage();
    layer.SetImage(image, kInvalidEventId, kInvalidEventId);
    layer.ApplyChanges({.h_addressable = kDisplayWidth, .v_addressable = kDisplayHeight});
    ASSERT_TRUE(layer.ResolvePendingImage(fences_.get(), ConfigStamp(1)));
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
    layer.SetImage(image, kInvalidEventId, kInvalidEventId);
    layer.ApplyChanges({.h_addressable = kDisplayWidth, .v_addressable = kDisplayHeight});
    ASSERT_TRUE(layer.ResolvePendingImage(fences_.get(), ConfigStamp(2)));
    ASSERT_TRUE(layer.ActivateLatestReadyImage());

    EXPECT_TRUE(layer.current_image());
    // layer is labeled current, so image cleanup will change the current config.
    EXPECT_TRUE(layer.CleanUpAllImages());
    EXPECT_FALSE(layer.current_image());

    image->EarlyRetire();

    current_layers.clear();
  }
}

}  // namespace display
