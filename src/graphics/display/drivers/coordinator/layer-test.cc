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

#include <functional>

#include <fbl/auto_lock.h>
#include <fbl/intrusive_single_list.h>
#include <fbl/ref_ptr.h>
#include <gtest/gtest.h>

#include "src/graphics/display/drivers/coordinator/fence.h"
#include "src/graphics/display/drivers/coordinator/image.h"
#include "src/graphics/display/drivers/coordinator/testing/base.h"
#include "src/graphics/display/drivers/fake/fake-display.h"
#include "src/graphics/display/lib/api-types/cpp/driver-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/event-id.h"
#include "src/graphics/display/lib/api-types/cpp/image-id.h"
#include "src/graphics/display/lib/api-types/cpp/layer-id.h"
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
        FakeDisplayEngine().ImportVmoImageForTesting(zx::vmo(0), 0);
    EXPECT_OK(import_result);
    EXPECT_NE(import_result.value(), display::kInvalidDriverImageId);

    static constexpr display::ImageMetadata image_metadata({
        .width = kDisplayWidth,
        .height = kDisplayHeight,
        .tiling_type = display::ImageTilingType::kLinear,
    });

    display::ImageId image_id = next_image_id_;
    ++next_image_id_;

    fbl::RefPtr<Image> image =
        fbl::AdoptRef(new Image(CoordinatorController(), image_metadata, image_id,
                                import_result.value(), nullptr, ClientId(1)));
    return image;
  }

  static void MakeLayerApplied(
      Layer& layer, fbl::DoublyLinkedList<LayerNode*>& applied_display_config_layer_list) {
    applied_display_config_layer_list.push_front(&layer.applied_display_config_list_node_);
  }

  // Returns a unique_ptr to `Layer` created on the driver dispatcher.
  //
  // The returned `Layer` is created with a custom deleter which destroys the
  // layer on the driver dispatcher, as required by the Layer destructor.
  //
  // Must not be called on the driver dispatcher, otherwise deadlock is guaranteed.
  std::unique_ptr<Layer, std::function<void(Layer*)>> CreateLayerForTest(
      display::LayerId layer_id) {
    auto deleter = [this](Layer* layer) {
      libsync::Completion completion;
      async::PostTask(CoordinatorController()->driver_dispatcher()->async_dispatcher(),
                      [&completion, layer]() {
                        delete layer;
                        completion.Signal();
                      });
      completion.Wait();
    };

    return RunOnDriverDispatcher<std::unique_ptr<Layer, std::function<void(Layer*)>>>(
        [this, layer_id, deleter = std::move(deleter)]() {
          return std::unique_ptr<Layer, decltype(deleter)>(
              new Layer(CoordinatorController(), layer_id), std::move(deleter));
        });
  }

  // Helper struct for `RunOnDriverDispatcher()`.  `std::optional<void>` is illegal, so we need our
  // own structs, one for each of the void and non-void cases.
  //
  // Note: T must be default-constructable.  This is for simplicity, and meets current use cases.
  template <typename T>
  struct RunOnDriverDispatcherResultHolder {
    T value;
  };

  // Specialization for void return type.
  template <>
  struct RunOnDriverDispatcherResultHolder<void> {};

  template <typename ReturnType>
  ReturnType RunOnDriverDispatcher(std::function<ReturnType()> func) {
    RunOnDriverDispatcherResultHolder<ReturnType> result;
    libsync::Completion completion;

    async::PostTask(CoordinatorController()->driver_dispatcher()->async_dispatcher(), [&]() {
      if constexpr (std::is_same_v<ReturnType, void>) {
        func();
      } else {
        result.value = func();
      }
      completion.Signal();
    });

    completion.Wait();
    if constexpr (!std::is_same_v<ReturnType, void>) {
      return std::move(result).value;
    }
  }

 protected:
  static constexpr uint32_t kDisplayWidth = 1024;
  static constexpr uint32_t kDisplayHeight = 600;

  std::unique_ptr<FenceCollection> fences_;

  display::ImageId next_image_id_ = display::ImageId(1);
};

TEST_F(LayerTest, PrimaryBasic) {
  std::unique_ptr layer = CreateLayerForTest(display::LayerId(1));

  RunOnDriverDispatcher<void>([&] {
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
    layer->ApplyChanges();
  });
}

TEST_F(LayerTest, CleanUpImage) {
  std::unique_ptr layer = CreateLayerForTest(display::LayerId(1));

  RunOnDriverDispatcher<void>([&] {
    fhdt::wire::ImageMetadata image_metadata = {
        .dimensions = {.width = kDisplayWidth, .height = kDisplayHeight},
        .tiling_type = fhdt::wire::kImageTilingTypeLinear};
    fuchsia_math::wire::RectU display_area = {.width = kDisplayWidth, .height = kDisplayHeight};
    layer->SetPrimaryConfig(image_metadata);
    layer->SetPrimaryPosition(fhdt::wire::CoordinateTransformation::kIdentity, display_area,
                              display_area);
    layer->SetPrimaryAlpha(fhdt::wire::AlphaMode::kDisable, 0);
  });

  auto displayed_image = CreateReadyImage();
  RunOnDriverDispatcher<void>([&] {
    layer->SetImage(displayed_image, display::kInvalidEventId);
    layer->ApplyChanges();
  });

  ASSERT_TRUE(RunOnDriverDispatcher<bool>(
      [&]() { return layer->ResolveDraftImage(fences_.get(), display::ConfigStamp(1)); }));

  zx::event event;
  ASSERT_OK(zx::event::create(0, &event));
  constexpr display::EventId kWaitFenceId(1);
  RunOnDriverDispatcher<void>([&]() { fences_->ImportEvent(std::move(event), kWaitFenceId); });
  auto fence_release = fit::defer([this, kWaitFenceId] {
    RunOnDriverDispatcher<void>([&]() { fences_->ReleaseEvent(kWaitFenceId); });
  });

  auto waiting_image = CreateReadyImage();
  layer->SetImage(waiting_image, kWaitFenceId);
  ASSERT_TRUE(RunOnDriverDispatcher<bool>(
      [&]() { return layer->ResolveDraftImage(fences_.get(), display::ConfigStamp(2)); }));

  auto draft_image = CreateReadyImage();
  layer->SetImage(draft_image, display::kInvalidEventId);

  ASSERT_TRUE(RunOnDriverDispatcher<bool>([&]() { return layer->ActivateLatestReadyImage(); }));

  EXPECT_TRUE(layer->applied_image());

  // Nothing should happen if image doesn't match.
  auto not_matching_image = CreateReadyImage();
  EXPECT_FALSE(RunOnDriverDispatcher<bool>([&]() {
    fbl::AutoLock lock(CoordinatorController()->mtx());
    CoordinatorController()->AssertMtxAliasHeld(*layer->mtx());
    return layer->CleanUpImage(*not_matching_image);
  }));
  EXPECT_TRUE(layer->applied_image());

  // Test cleaning up a waiting image.
  EXPECT_FALSE(RunOnDriverDispatcher<bool>([&]() {
    fbl::AutoLock lock(CoordinatorController()->mtx());
    CoordinatorController()->AssertMtxAliasHeld(*layer->mtx());
    return layer->CleanUpImage(*waiting_image);
  }));
  EXPECT_TRUE(layer->applied_image());

  // Test cleaning up a draft image.
  EXPECT_FALSE(RunOnDriverDispatcher<bool>([&]() {
    fbl::AutoLock lock(CoordinatorController()->mtx());
    CoordinatorController()->AssertMtxAliasHeld(*layer->mtx());
    return layer->CleanUpImage(*draft_image);
  }));
  EXPECT_TRUE(layer->applied_image());

  // Test cleaning up the associated image.
  //
  // The layer is not in a display's applied configuration list. So, cleaning up
  // the layer's image doesn't change the applied config.
  EXPECT_FALSE(RunOnDriverDispatcher<bool>([&]() {
    fbl::AutoLock lock(CoordinatorController()->mtx());
    CoordinatorController()->AssertMtxAliasHeld(*layer->mtx());
    return layer->CleanUpImage(*displayed_image);
  }));
  EXPECT_FALSE(layer->applied_image());
}

TEST_F(LayerTest, CleanUpImage_CheckConfigChange) {
  fbl::DoublyLinkedList<LayerNode*> applied_layers;

  std::unique_ptr layer = CreateLayerForTest(display::LayerId(1));

  RunOnDriverDispatcher<void>([&] {
    fhdt::wire::ImageMetadata image_metadata = {
        .dimensions = {.width = kDisplayWidth, .height = kDisplayHeight},
        .tiling_type = fhdt::wire::kImageTilingTypeLinear};
    fuchsia_math::wire::RectU display_area = {.width = kDisplayWidth, .height = kDisplayHeight};
    layer->SetPrimaryConfig(image_metadata);
    layer->SetPrimaryPosition(fhdt::wire::CoordinateTransformation::kIdentity, display_area,
                              display_area);
    layer->SetPrimaryAlpha(fhdt::wire::AlphaMode::kDisable, 0);
  });

  // Clean up images, which doesn't change the applied config.
  {
    auto image = CreateReadyImage();
    RunOnDriverDispatcher<void>([&] {
      layer->SetImage(image, display::kInvalidEventId);
      layer->ApplyChanges();
    });
    ASSERT_TRUE(RunOnDriverDispatcher<bool>(
        [&]() { return layer->ResolveDraftImage(fences_.get(), display::ConfigStamp(1)); }));
    ASSERT_TRUE(RunOnDriverDispatcher<bool>([&]() { return layer->ActivateLatestReadyImage(); }));

    EXPECT_TRUE(layer->applied_image());
    // The layer is not in a display's applied configuration list. So, cleaning
    // up the layer's image doesn't change the applied config.
    EXPECT_FALSE(RunOnDriverDispatcher<bool>([&]() {
      fbl::AutoLock lock(CoordinatorController()->mtx());
      CoordinatorController()->AssertMtxAliasHeld(*layer->mtx());
      return layer->CleanUpImage(*image);
    }));
    EXPECT_FALSE(layer->applied_image());
  }

  // Clean up images, which changes the applied config.
  {
    MakeLayerApplied(*layer, applied_layers);

    auto image = CreateReadyImage();
    RunOnDriverDispatcher<void>([&] {
      layer->SetImage(image, display::kInvalidEventId);
      layer->ApplyChanges();
    });
    ASSERT_TRUE(RunOnDriverDispatcher<bool>(
        [&]() { return layer->ResolveDraftImage(fences_.get(), display::ConfigStamp(2)); }));
    ASSERT_TRUE(RunOnDriverDispatcher<bool>([&]() { return layer->ActivateLatestReadyImage(); }));

    EXPECT_TRUE(layer->applied_image());

    // The layer is in a display's applied configuration list. So, cleaning up
    // the layer's image changes the applied config.
    EXPECT_TRUE(RunOnDriverDispatcher<bool>([&]() {
      fbl::AutoLock lock(CoordinatorController()->mtx());
      CoordinatorController()->AssertMtxAliasHeld(*layer->mtx());
      return layer->CleanUpImage(*image);
    }));
    EXPECT_FALSE(layer->applied_image());

    applied_layers.clear();
  }
}

TEST_F(LayerTest, CleanUpAllImages) {
  std::unique_ptr layer = CreateLayerForTest(display::LayerId(1));

  RunOnDriverDispatcher<void>([&] {
    fhdt::wire::ImageMetadata image_metadata = {
        .dimensions = {.width = kDisplayWidth, .height = kDisplayHeight},
        .tiling_type = fhdt::wire::kImageTilingTypeLinear};
    fuchsia_math::wire::RectU display_area = {.width = kDisplayWidth, .height = kDisplayHeight};
    layer->SetPrimaryConfig(image_metadata);
    layer->SetPrimaryPosition(fhdt::wire::CoordinateTransformation::kIdentity, display_area,
                              display_area);
    layer->SetPrimaryAlpha(fhdt::wire::AlphaMode::kDisable, 0);
  });

  auto displayed_image = CreateReadyImage();
  RunOnDriverDispatcher<void>([&] {
    layer->SetImage(displayed_image, display::kInvalidEventId);
    layer->ApplyChanges();
  });
  ASSERT_TRUE(RunOnDriverDispatcher<bool>(
      [&]() { return layer->ResolveDraftImage(fences_.get(), display::ConfigStamp(1)); }));

  zx::event event;
  ASSERT_OK(zx::event::create(0, &event));
  constexpr display::EventId kWaitFenceId(1);
  RunOnDriverDispatcher<void>([&]() { fences_->ImportEvent(std::move(event), kWaitFenceId); });
  auto fence_release = fit::defer([this, kWaitFenceId] {
    RunOnDriverDispatcher<void>([&]() { fences_->ReleaseEvent(kWaitFenceId); });
  });

  auto waiting_image = CreateReadyImage();
  RunOnDriverDispatcher<void>([&] { layer->SetImage(waiting_image, kWaitFenceId); });
  ASSERT_TRUE(RunOnDriverDispatcher<bool>(
      [&]() { return layer->ResolveDraftImage(fences_.get(), display::ConfigStamp(2)); }));

  auto draft_image = CreateReadyImage();
  RunOnDriverDispatcher<void>([&] { layer->SetImage(draft_image, display::kInvalidEventId); });

  ASSERT_TRUE(RunOnDriverDispatcher<bool>([&]() { return layer->ActivateLatestReadyImage(); }));

  // The layer is not in a display's applied configuration list. So, cleaning
  // up the layer's image doesn't change the applied config.
  EXPECT_FALSE(RunOnDriverDispatcher<bool>([&]() {
    fbl::AutoLock lock(CoordinatorController()->mtx());
    CoordinatorController()->AssertMtxAliasHeld(*layer->mtx());
    return layer->CleanUpAllImages();
  }));
  EXPECT_FALSE(layer->applied_image());
}

TEST_F(LayerTest, CleanUpAllImages_CheckConfigChange) {
  fbl::DoublyLinkedList<LayerNode*> applied_layers;

  std::unique_ptr layer = CreateLayerForTest(display::LayerId(1));

  RunOnDriverDispatcher<void>([&] {
    fhdt::wire::ImageMetadata image_config = {
        .dimensions = {.width = kDisplayWidth, .height = kDisplayHeight},
        .tiling_type = fhdt::wire::kImageTilingTypeLinear};
    fuchsia_math::wire::RectU display_area = {.width = kDisplayWidth, .height = kDisplayHeight};
    layer->SetPrimaryConfig(image_config);
    layer->SetPrimaryPosition(fhdt::wire::CoordinateTransformation::kIdentity, display_area,
                              display_area);
    layer->SetPrimaryAlpha(fhdt::wire::AlphaMode::kDisable, 0);
  });

  // Clean up all images, which doesn't change the applied config.
  {
    auto image = CreateReadyImage();
    RunOnDriverDispatcher<void>([&] {
      layer->SetImage(image, display::kInvalidEventId);
      layer->ApplyChanges();
    });
    ASSERT_TRUE(RunOnDriverDispatcher<bool>(
        [&]() { return layer->ResolveDraftImage(fences_.get(), display::ConfigStamp(1)); }));
    ASSERT_TRUE(RunOnDriverDispatcher<bool>([&]() { return layer->ActivateLatestReadyImage(); }));

    EXPECT_TRUE(layer->applied_image());
    // The layer is not in a display's applied configuration list. So, cleaning
    // up the layer's image doesn't change the applied config.
    EXPECT_FALSE(RunOnDriverDispatcher<bool>([&]() {
      fbl::AutoLock lock(CoordinatorController()->mtx());
      CoordinatorController()->AssertMtxAliasHeld(*layer->mtx());
      return layer->CleanUpAllImages();
    }));
    EXPECT_FALSE(layer->applied_image());
  }

  // Clean up all images, which changes the applied config.
  {
    MakeLayerApplied(*layer, applied_layers);

    auto image = CreateReadyImage();
    RunOnDriverDispatcher<void>([&] {
      layer->SetImage(image, display::kInvalidEventId);
      layer->ApplyChanges();
    });
    ASSERT_TRUE(RunOnDriverDispatcher<bool>(
        [&]() { return layer->ResolveDraftImage(fences_.get(), display::ConfigStamp(2)); }));
    ASSERT_TRUE(RunOnDriverDispatcher<bool>([&]() { return layer->ActivateLatestReadyImage(); }));

    EXPECT_TRUE(layer->applied_image());
    // The layer is in a display's applied configuration list. So, cleaning up
    // the layer's image changes the applied config.
    EXPECT_TRUE(RunOnDriverDispatcher<bool>([&]() {
      fbl::AutoLock lock(CoordinatorController()->mtx());
      CoordinatorController()->AssertMtxAliasHeld(*layer->mtx());
      return layer->CleanUpAllImages();
    }));
    EXPECT_FALSE(layer->applied_image());

    applied_layers.clear();
  }
}

}  // namespace display_coordinator
