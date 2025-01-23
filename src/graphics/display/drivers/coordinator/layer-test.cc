// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/layer.h"

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fidl/fuchsia.math/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/fit/defer.h>
#include <lib/sync/cpp/completion.h>
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
    layer_waiting_image_allocator_ = std::make_unique<LayerWaitingImageAllocator>(/*max_slabs*/ 1);
  }

  LayerWaitingImageAllocator& layer_waiting_image_allocator() const {
    return *layer_waiting_image_allocator_;
  }

  fbl::RefPtr<Image> CreateReadyImage() {
    zx::result<display::DriverImageId> import_result =
        FakeDisplayEngine().ImportVmoImageForTesting(zx::vmo(0), 0);
    EXPECT_OK(import_result);
    EXPECT_NE(import_result.value(), display::kInvalidDriverImageId);

    static constexpr display::ImageMetadata image_metadata({
        .width = kDisplayWidth,
        .height = kDisplayHeight,
        .tiling_type = display::ImageTilingType::kLinear,
    });
    fbl::RefPtr<Image> image = fbl::AdoptRef(new Image(
        CoordinatorController(), image_metadata, import_result.value(), nullptr, ClientId(1)));
    image->id = next_image_id_++;
    image->Acquire();
    return image;
  }

  static void MakeLayerCurrent(Layer& layer, fbl::DoublyLinkedList<LayerNode*>& current_layers) {
    current_layers.push_front(&layer.current_node_);
  }

  // Helper method that returns a unique_ptr with a custom deleter which destroys the layer on the
  // controller loop, as required by the Layer destructor.
  //
  // Must not be called on the controller's client loop, otherwise deadlock is guaranteed.
  std::unique_ptr<Layer, std::function<void(Layer*)>> CreateLayerForTest(
      display::DriverLayerId layer_id) {
    auto deleter = [this](Layer* layer) {
      libsync::Completion completion;
      async::PostTask(CoordinatorController()->client_dispatcher()->async_dispatcher(),
                      [&completion, layer]() {
                        delete layer;
                        completion.Signal();
                      });
      completion.Wait();
    };
    return std::unique_ptr<Layer, decltype(deleter)>(
        new Layer(CoordinatorController(), layer_id, &layer_waiting_image_allocator()),
        std::move(deleter));
  }

  // Helper that waits for asynchronous completion of `layer.ActivateLatestReadyImage()`, which is
  // invoked on the controller's client dispatcher (as it would be while handling a FIDL request).
  //
  // Must not be called on the controller's client loop, otherwise deadlock is guaranteed.
  bool ActivateLatestReadyImageOnControllerLoop(Layer& layer) {
    std::optional<bool> return_value;
    libsync::Completion completion;

    async::PostTask(CoordinatorController()->client_dispatcher()->async_dispatcher(), [&]() {
      return_value = layer.ActivateLatestReadyImage();
      completion.Signal();
    });
    completion.Wait();
    ZX_ASSERT(return_value.has_value());
    return return_value.value();
  }

  // Helper that waits for asynchronous completion of `layer.ResolvePendingImage()`, which is
  // invoked on the controller's client dispatcher (as it would be while handling a FIDL request).
  //
  // Must not be called on the controller's client loop, otherwise deadlock is guaranteed.
  bool ResolvePendingImageOnControllerLoop(Layer& layer, FenceCollection* fences,
                                           display::ConfigStamp stamp) {
    std::optional<bool> return_value;
    libsync::Completion completion;

    async::PostTask(CoordinatorController()->client_dispatcher()->async_dispatcher(), [&]() {
      return_value = layer.ResolvePendingImage(fences, stamp);
      completion.Signal();
    });
    completion.Wait();
    ZX_ASSERT(return_value.has_value());
    return return_value.value();
  }

  // Helper that waits for asynchronous completion of `layer.CleanUpAllImages()`, which is invoked
  // on the controller's client dispatcher (as it would be while handling a FIDL request).
  //
  // Must not be called on the controller's client loop, otherwise deadlock is guaranteed.
  bool CleanUpAllImagesOnControllerLoop(Layer& layer) {
    std::optional<bool> return_value;
    libsync::Completion completion;

    async::PostTask(CoordinatorController()->client_dispatcher()->async_dispatcher(), [&]() {
      fbl::AutoLock lock(CoordinatorController()->mtx());
      CoordinatorController()->AssertMtxAliasHeld(*layer.mtx());
      return_value = layer.CleanUpAllImages();
      completion.Signal();
    });
    completion.Wait();
    ZX_ASSERT(return_value.has_value());
    return return_value.value();
  }

  // Helper that waits for asynchronous completion of `layer.CleanUpImage()`, which is invoked
  // on the controller's client dispatcher (as it would be while handling a FIDL request).
  //
  // Must not be called on the controller's client loop, otherwise deadlock is guaranteed.
  bool CleanUpImageOnControllerLoop(Layer& layer, const Image& image) {
    std::optional<bool> return_value;
    libsync::Completion completion;

    async::PostTask(CoordinatorController()->client_dispatcher()->async_dispatcher(), [&]() {
      fbl::AutoLock lock(CoordinatorController()->mtx());
      CoordinatorController()->AssertMtxAliasHeld(*layer.mtx());
      return_value = layer.CleanUpImage(image);
      completion.Signal();
    });
    completion.Wait();
    ZX_ASSERT(return_value.has_value());
    return return_value.value();
  }

  // Helper that waits for asynchronous completion of `fences->ImportEvent()`, which is invoked
  // on the controller's client dispatcher (as it would be while handling a FIDL request).
  // This is necessary because there are ASSERTs that verify that the fence is being used on the
  // loop it was created on.
  //
  // Must not be called on the controller's client loop, otherwise deadlock is guaranteed.
  void ImportEventOnControllerLoop(FenceCollection* fences, zx::event event,
                                   display::EventId event_id) {
    libsync::Completion completion;
    async::PostTask(CoordinatorController()->client_dispatcher()->async_dispatcher(), [&]() {
      fences_->ImportEvent(std::move(event), event_id);
      completion.Signal();
    });
    completion.Wait();
  }

  // Helper that waits for asynchronous completion of `fences->ReleaseEvent()`, which is invoked
  // on the controller's client dispatcher (as it would be while handling a FIDL request).
  // This is necessary because there are ASSERTs that verify that the fence is being used on the
  // loop it was created on.
  //
  // Must not be called on the controller's client loop, otherwise deadlock is guaranteed.
  void ReleaseEventOnControllerLoop(FenceCollection* fences, display::EventId event_id) {
    libsync::Completion completion;
    async::PostTask(CoordinatorController()->client_dispatcher()->async_dispatcher(), [&]() {
      fences_->ReleaseEvent(event_id);
      completion.Signal();
    });
    completion.Wait();
  }

 protected:
  static constexpr uint32_t kDisplayWidth = 1024;
  static constexpr uint32_t kDisplayHeight = 600;

  std::unique_ptr<FenceCollection> fences_;
  std::unique_ptr<LayerWaitingImageAllocator> layer_waiting_image_allocator_;

  display::ImageId next_image_id_ = display::ImageId(1);
};

TEST_F(LayerTest, PrimaryBasic) {
  std::unique_ptr layer = CreateLayerForTest(display::DriverLayerId(1));

  fhdt::wire::ImageMetadata image_metadata = {
      .dimensions = {.width = kDisplayWidth, .height = kDisplayHeight},
      .tiling_type = fhdt::wire::kImageTilingTypeLinear};
  fuchsia_math::wire::RectU display_area = {.width = kDisplayWidth, .height = kDisplayHeight};
  layer->SetPrimaryConfig(image_metadata);
  layer->SetPrimaryPosition(fhdt::wire::CoordinateTransformation::kIdentity, display_area,
                            display_area);
  layer->SetPrimaryAlpha(fhdt::wire::AlphaMode::kDisable, 0);
  auto image = CreateReadyImage();
  layer->SetImage(image, display::kInvalidEventId);
  layer->ApplyChanges({.h_addressable = kDisplayWidth, .v_addressable = kDisplayHeight});
}

TEST_F(LayerTest, CleanUpImage) {
  std::unique_ptr layer = CreateLayerForTest(display::DriverLayerId(1));

  fhdt::wire::ImageMetadata image_metadata = {
      .dimensions = {.width = kDisplayWidth, .height = kDisplayHeight},
      .tiling_type = fhdt::wire::kImageTilingTypeLinear};
  fuchsia_math::wire::RectU display_area = {.width = kDisplayWidth, .height = kDisplayHeight};
  layer->SetPrimaryConfig(image_metadata);
  layer->SetPrimaryPosition(fhdt::wire::CoordinateTransformation::kIdentity, display_area,
                            display_area);
  layer->SetPrimaryAlpha(fhdt::wire::AlphaMode::kDisable, 0);

  auto displayed_image = CreateReadyImage();
  layer->SetImage(displayed_image, display::kInvalidEventId);
  layer->ApplyChanges({.h_addressable = kDisplayWidth, .v_addressable = kDisplayHeight});
  ASSERT_TRUE(ResolvePendingImageOnControllerLoop(*layer, fences_.get(), display::ConfigStamp(1)));

  zx::event event;
  ASSERT_OK(zx::event::create(0, &event));
  constexpr display::EventId kWaitFenceId(1);
  ImportEventOnControllerLoop(fences_.get(), std::move(event), kWaitFenceId);
  auto fence_release = fit::defer(
      [this, kWaitFenceId] { ReleaseEventOnControllerLoop(fences_.get(), kWaitFenceId); });

  auto waiting_image = CreateReadyImage();
  layer->SetImage(waiting_image, kWaitFenceId);
  ASSERT_TRUE(ResolvePendingImageOnControllerLoop(*layer, fences_.get(), display::ConfigStamp(2)));

  auto pending_image = CreateReadyImage();
  layer->SetImage(pending_image, display::kInvalidEventId);

  ASSERT_TRUE(ActivateLatestReadyImageOnControllerLoop(*layer));

  EXPECT_TRUE(layer->current_image());
  // pending / waiting images are still busy.
  EXPECT_FALSE(pending_image->Acquire());
  EXPECT_FALSE(waiting_image->Acquire());

  // Nothing should happen if image doesn't match.
  auto not_matching_image = CreateReadyImage();
  EXPECT_FALSE(CleanUpImageOnControllerLoop(*layer, *not_matching_image));
  EXPECT_TRUE(layer->current_image());
  EXPECT_FALSE(pending_image->Acquire());
  EXPECT_FALSE(waiting_image->Acquire());

  // Test cleaning up a waiting image.
  EXPECT_FALSE(CleanUpImageOnControllerLoop(*layer, *waiting_image));
  EXPECT_TRUE(layer->current_image());
  EXPECT_FALSE(pending_image->Acquire());
  // waiting_image should be released.
  EXPECT_TRUE(waiting_image->Acquire());

  // Test cleaning up a pending image.
  EXPECT_FALSE(CleanUpImageOnControllerLoop(*layer, *pending_image));
  EXPECT_TRUE(layer->current_image());
  // pending_image should be released.
  EXPECT_TRUE(pending_image->Acquire());

  // Test cleaning up the displayed image.
  // layer is not labeled current, so it doesn't change the current config.
  EXPECT_FALSE(CleanUpImageOnControllerLoop(*layer, *displayed_image));
  EXPECT_FALSE(layer->current_image());

  // Teardown. Images must be unused (retired) when destroyed.
  displayed_image->EarlyRetire();
  not_matching_image->EarlyRetire();
  waiting_image->EarlyRetire();
  pending_image->EarlyRetire();
}

TEST_F(LayerTest, CleanUpImage_CheckConfigChange) {
  fbl::DoublyLinkedList<LayerNode*> current_layers;

  std::unique_ptr layer = CreateLayerForTest(display::DriverLayerId(1));

  fhdt::wire::ImageMetadata image_metadata = {
      .dimensions = {.width = kDisplayWidth, .height = kDisplayHeight},
      .tiling_type = fhdt::wire::kImageTilingTypeLinear};
  fuchsia_math::wire::RectU display_area = {.width = kDisplayWidth, .height = kDisplayHeight};
  layer->SetPrimaryConfig(image_metadata);
  layer->SetPrimaryPosition(fhdt::wire::CoordinateTransformation::kIdentity, display_area,
                            display_area);
  layer->SetPrimaryAlpha(fhdt::wire::AlphaMode::kDisable, 0);

  // Clean up images, which doesn't change the current config.
  {
    auto image = CreateReadyImage();
    layer->SetImage(image, display::kInvalidEventId);
    layer->ApplyChanges({.h_addressable = kDisplayWidth, .v_addressable = kDisplayHeight});
    ASSERT_TRUE(
        ResolvePendingImageOnControllerLoop(*layer, fences_.get(), display::ConfigStamp(1)));
    ASSERT_TRUE(ActivateLatestReadyImageOnControllerLoop(*layer));

    EXPECT_TRUE(layer->current_image());
    // layer is not labeled current, so image cleanup doesn't change the current
    // config.
    EXPECT_FALSE(CleanUpImageOnControllerLoop(*layer, *image));
    EXPECT_FALSE(layer->current_image());

    image->EarlyRetire();
  }

  // Clean up images, which changes the current config.
  {
    MakeLayerCurrent(*layer, current_layers);

    auto image = CreateReadyImage();
    layer->SetImage(image, display::kInvalidEventId);
    layer->ApplyChanges({.h_addressable = kDisplayWidth, .v_addressable = kDisplayHeight});
    ASSERT_TRUE(
        ResolvePendingImageOnControllerLoop(*layer, fences_.get(), display::ConfigStamp(2)));
    ASSERT_TRUE(ActivateLatestReadyImageOnControllerLoop(*layer));

    EXPECT_TRUE(layer->current_image());
    // layer is labeled current, so image cleanup will change the current config.
    EXPECT_TRUE(CleanUpImageOnControllerLoop(*layer, *image));
    EXPECT_FALSE(layer->current_image());

    image->EarlyRetire();

    current_layers.clear();
  }
}

TEST_F(LayerTest, CleanUpAllImages) {
  std::unique_ptr layer = CreateLayerForTest(display::DriverLayerId(1));

  fhdt::wire::ImageMetadata image_metadata = {
      .dimensions = {.width = kDisplayWidth, .height = kDisplayHeight},
      .tiling_type = fhdt::wire::kImageTilingTypeLinear};
  fuchsia_math::wire::RectU display_area = {.width = kDisplayWidth, .height = kDisplayHeight};
  layer->SetPrimaryConfig(image_metadata);
  layer->SetPrimaryPosition(fhdt::wire::CoordinateTransformation::kIdentity, display_area,
                            display_area);
  layer->SetPrimaryAlpha(fhdt::wire::AlphaMode::kDisable, 0);

  auto displayed_image = CreateReadyImage();
  layer->SetImage(displayed_image, display::kInvalidEventId);
  layer->ApplyChanges({.h_addressable = kDisplayWidth, .v_addressable = kDisplayHeight});
  ASSERT_TRUE(ResolvePendingImageOnControllerLoop(*layer, fences_.get(), display::ConfigStamp(1)));

  zx::event event;
  ASSERT_OK(zx::event::create(0, &event));
  constexpr display::EventId kWaitFenceId(1);
  ImportEventOnControllerLoop(fences_.get(), std::move(event), kWaitFenceId);
  auto fence_release = fit::defer(
      [this, kWaitFenceId] { ReleaseEventOnControllerLoop(fences_.get(), kWaitFenceId); });

  auto waiting_image = CreateReadyImage();
  layer->SetImage(waiting_image, kWaitFenceId);
  ASSERT_TRUE(ResolvePendingImageOnControllerLoop(*layer, fences_.get(), display::ConfigStamp(2)));

  auto pending_image = CreateReadyImage();
  layer->SetImage(pending_image, display::kInvalidEventId);

  ASSERT_TRUE(ActivateLatestReadyImageOnControllerLoop(*layer));

  // layer is not labeled current, so it doesn't change the current config.
  EXPECT_FALSE(CleanUpAllImagesOnControllerLoop(*layer));
  EXPECT_FALSE(layer->current_image());
  // pending_image should be released.
  EXPECT_TRUE(pending_image->Acquire());
  // waiting_image should be released.
  EXPECT_TRUE(waiting_image->Acquire());

  // Teardown. Images must be unused (retired) when destroyed.
  displayed_image->EarlyRetire();
  waiting_image->EarlyRetire();
  pending_image->EarlyRetire();
}

TEST_F(LayerTest, CleanUpAllImages_CheckConfigChange) {
  fbl::DoublyLinkedList<LayerNode*> current_layers;

  std::unique_ptr layer = CreateLayerForTest(display::DriverLayerId(1));

  fhdt::wire::ImageMetadata image_config = {
      .dimensions = {.width = kDisplayWidth, .height = kDisplayHeight},
      .tiling_type = fhdt::wire::kImageTilingTypeLinear};
  fuchsia_math::wire::RectU display_area = {.width = kDisplayWidth, .height = kDisplayHeight};
  layer->SetPrimaryConfig(image_config);
  layer->SetPrimaryPosition(fhdt::wire::CoordinateTransformation::kIdentity, display_area,
                            display_area);
  layer->SetPrimaryAlpha(fhdt::wire::AlphaMode::kDisable, 0);

  // Clean up all images, which doesn't change the current config.
  {
    auto image = CreateReadyImage();
    layer->SetImage(image, display::kInvalidEventId);
    layer->ApplyChanges({.h_addressable = kDisplayWidth, .v_addressable = kDisplayHeight});
    ASSERT_TRUE(
        ResolvePendingImageOnControllerLoop(*layer, fences_.get(), display::ConfigStamp(1)));
    ASSERT_TRUE(ActivateLatestReadyImageOnControllerLoop(*layer));

    EXPECT_TRUE(layer->current_image());
    // layer is not labeled current, so image cleanup doesn't change the current
    // config.
    EXPECT_FALSE(CleanUpAllImagesOnControllerLoop(*layer));
    EXPECT_FALSE(layer->current_image());

    image->EarlyRetire();
  }

  // Clean up all images, which changes the current config.
  {
    MakeLayerCurrent(*layer, current_layers);

    auto image = CreateReadyImage();
    layer->SetImage(image, display::kInvalidEventId);
    layer->ApplyChanges({.h_addressable = kDisplayWidth, .v_addressable = kDisplayHeight});
    ASSERT_TRUE(
        ResolvePendingImageOnControllerLoop(*layer, fences_.get(), display::ConfigStamp(2)));
    ASSERT_TRUE(ActivateLatestReadyImageOnControllerLoop(*layer));

    EXPECT_TRUE(layer->current_image());
    // layer is labeled current, so image cleanup will change the current config.
    EXPECT_TRUE(CleanUpAllImagesOnControllerLoop(*layer));
    EXPECT_FALSE(layer->current_image());

    image->EarlyRetire();

    current_layers.clear();
  }
}

}  // namespace display_coordinator
