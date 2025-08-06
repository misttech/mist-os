// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/layer.h"

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fidl/fuchsia.math/cpp/wire.h>
#include <lib/driver/logging/cpp/logger.h>
#include <zircon/assert.h>

#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <optional>
#include <utility>

#include <fbl/auto_lock.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/ref_ptr.h>

#include "src/graphics/display/drivers/coordinator/controller.h"
#include "src/graphics/display/drivers/coordinator/fence.h"
#include "src/graphics/display/drivers/coordinator/image.h"
#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/driver-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/event-id.h"
#include "src/graphics/display/lib/api-types/cpp/image-metadata.h"
#include "src/graphics/display/lib/api-types/cpp/layer-id.h"
#include "src/graphics/display/lib/api-types/cpp/rectangle.h"

namespace display_coordinator {

namespace {

display::DriverLayer CreatePlaceholderDriverLayer() {
  return display::DriverLayer({
      .display_destination = display::Rectangle({.x = 0, .y = 0, .width = 1, .height = 1}),
      .image_source = display::Rectangle({.x = 0, .y = 0, .width = 0, .height = 0}),
      .image_id = display::kInvalidDriverImageId,
      .image_metadata = display::ImageMetadata({
          .width = 0,
          .height = 0,
          .tiling_type = display::ImageTilingType::kLinear,
      }),
      .fallback_color = display::Color({
          .format = display::PixelFormat::kR8G8B8A8,
          .bytes = {{0, 0, 0, 0, 0, 0, 0, 0}},
      }),
  });
}

}  // namespace

static_assert(WaitingImageList::kMaxSize ==
                  fuchsia_hardware_display::wire::kMaxWaitingImagesPerLayer,
              "Violation of fuchsia.hardware.display.Coordinator API contract.");

Layer::Layer(Controller* controller, display::LayerId id)
    : IdMappable(id),
      controller_(*controller),
      draft_layer_config_(CreatePlaceholderDriverLayer()),
      applied_layer_config_(CreatePlaceholderDriverLayer()) {
  ZX_DEBUG_ASSERT(controller != nullptr);

  draft_layer_config_differs_from_applied_ = false;

  draft_display_config_list_node_.layer = this;
  applied_display_config_list_node_.layer = this;
  is_skipped_ = false;
}

Layer::~Layer() {
  ZX_DEBUG_ASSERT(!in_use());
  ZX_DEBUG_ASSERT(controller_.IsRunningOnDriverDispatcher());
  waiting_images_.RemoveAllImages();
}

bool Layer::ResolveDraftLayerProperties() {
  ZX_DEBUG_ASSERT(controller_.IsRunningOnDriverDispatcher());

  // If the layer's image configuration changed, get rid of any current images
  if (draft_image_config_gen_ != applied_image_config_gen_) {
    applied_image_config_gen_ = draft_image_config_gen_;

    if (draft_image_ == nullptr) {
      fdf::error("Tried to apply configuration with missing image");
      return false;
    }

    waiting_images_.RemoveAllImages();
    applied_image_ = nullptr;
  }
  return true;
}

bool Layer::ResolveDraftImage(FenceCollection* fences, display::ConfigStamp stamp) {
  ZX_DEBUG_ASSERT(controller_.IsRunningOnDriverDispatcher());

  if (draft_image_ != nullptr) {
    auto wait_fence = fences->GetFence(draft_image_wait_event_id_);
    draft_image_wait_event_id_ = display::kInvalidEventId;
    if (auto result = waiting_images_.PushImage(std::move(draft_image_), std::move(wait_fence));
        result.is_error()) {
      return false;
    }
  }

  // This relates to the strategy used by `Client::ApplyConfig()` to compute the vsync config stamp
  // that will be returned to the client (see more detailed comment there). The subtlety is that we
  // cannot set the image's stamp above (within the scope of `if (draft_image_) {`); it must be
  // done here.
  //
  // This is because the same image can appear in multiple configs. If we only set the stamp when
  // the image moves from `draft_image_` to `waiting_images_`, then we would improperly compute
  // the vsync config stamp sent in `CoordinatorListener.OnVsync`. Consequently, the client would
  // improperly compute whether a particular image is free to reuse.
  waiting_images_.UpdateLatestClientConfigStamp(stamp);
  return true;
}

void Layer::ApplyChanges() {
  if (!draft_layer_config_differs_from_applied_) {
    return;
  }

  const display::Rectangle& image_source = draft_layer_config_.image_source();
  const bool is_solid_color_fill = image_source.width() == 0 && image_source.height() == 0;
  if (is_solid_color_fill) {
    applied_image_ = nullptr;
    waiting_images_.RemoveAllImages();
  }

  applied_layer_config_ = display::DriverLayer({
      .display_destination = draft_layer_config_.display_destination(),
      .image_source = draft_layer_config_.image_source(),
      .image_id = applied_image_ ? applied_image_->driver_id() : display::kInvalidDriverImageId,
      .image_metadata = draft_layer_config_.image_metadata(),
      .fallback_color = draft_layer_config_.fallback_color(),
      .alpha_mode = draft_layer_config_.alpha_mode(),
      .alpha_coefficient = draft_layer_config_.alpha_coefficient(),
      .image_source_transformation = draft_layer_config_.image_source_transformation(),
  });
  draft_layer_config_differs_from_applied_ = false;
}

void Layer::DiscardChanges() {
  draft_image_config_gen_ = applied_image_config_gen_;
  draft_image_ = nullptr;
  if (draft_layer_config_differs_from_applied_) {
    draft_layer_config_ = applied_layer_config_;
    draft_layer_config_differs_from_applied_ = false;
  }
}

bool Layer::CleanUpAllImages() {
  ZX_DEBUG_ASSERT(controller_.IsRunningOnDriverDispatcher());

  RetireDraftImage();

  waiting_images_.RemoveAllImages();

  return RetireAppliedImage();
}

bool Layer::CleanUpImage(const Image& image) {
  if (draft_image_.get() == &image) {
    RetireDraftImage();
  }

  RetireWaitingImage(image);

  if (applied_image_.get() == &image) {
    return RetireAppliedImage();
  }
  return false;
}

std::optional<display::ConfigStamp> Layer::GetCurrentClientConfigStamp() const {
  if (applied_image_ != nullptr) {
    return applied_image_->latest_client_config_stamp();
  }
  return std::nullopt;
}

bool Layer::ActivateLatestReadyImage() {
  ZX_DEBUG_ASSERT(controller_.IsRunningOnDriverDispatcher());

  fbl::RefPtr<Image> newest_ready_image = waiting_images_.PopNewestReadyImage();
  if (!newest_ready_image) {
    return false;
  }
  ZX_DEBUG_ASSERT(applied_image_ == nullptr || (newest_ready_image->latest_client_config_stamp() >
                                                applied_image_->latest_client_config_stamp()));

  applied_image_ = std::move(newest_ready_image);
  applied_layer_config_ = display::DriverLayer({
      .display_destination = applied_layer_config_.display_destination(),
      .image_source = applied_layer_config_.image_source(),
      .image_id = applied_image_->driver_id(),
      .image_metadata = applied_layer_config_.image_metadata(),
      .fallback_color = applied_layer_config_.fallback_color(),
      .alpha_mode = applied_layer_config_.alpha_mode(),
      .alpha_coefficient = applied_layer_config_.alpha_coefficient(),
      .image_source_transformation = applied_layer_config_.image_source_transformation(),
  });

  // TODO(costan): `applied_layer_config_` is updated without updating
  // `draft_layer_config_differs_from_applied_`. Is it guaranteed that the
  // draft config has changed enough, or will this cause trouble?

  return true;
}

bool Layer::AppendToConfigLayerList(fbl::DoublyLinkedList<LayerNode*>& config_layer_list) {
  if (draft_display_config_list_node_.InContainer()) {
    return false;
  }

  config_layer_list.push_back(&draft_display_config_list_node_);
  return true;
}

void Layer::SetPrimaryConfig(display::ImageMetadata image_metadata) {
  const display::Rectangle image_area = {{.x = 0,
                                          .y = 0,
                                          .width = image_metadata.dimensions().width(),
                                          .height = image_metadata.dimensions().height()}};
  draft_layer_config_ = display::DriverLayer({
      .display_destination = image_area,
      .image_source = image_area,
      .image_id = display::kInvalidDriverImageId,
      .image_metadata = display::ImageMetadata(image_metadata),
      .fallback_color = display::Color({
          .format = display::PixelFormat::kR8G8B8A8,
          .bytes = {{0, 0, 0, 0, 0, 0, 0, 0}},
      }),
      .alpha_mode = draft_layer_config_.alpha_mode(),
      .alpha_coefficient = draft_layer_config_.alpha_coefficient(),
      .image_source_transformation = draft_layer_config_.image_source_transformation(),
  });
  draft_layer_config_differs_from_applied_ = true;

  ++draft_image_config_gen_;
  draft_image_ = nullptr;
}

void Layer::SetPrimaryPosition(display::CoordinateTransformation image_source_transformation,
                               display::Rectangle image_source,
                               display::Rectangle display_destination) {
  draft_layer_config_ = display::DriverLayer({
      .display_destination = display_destination,
      .image_source = image_source,
      .image_id = draft_layer_config_.image_id(),
      .image_metadata = draft_layer_config_.image_metadata(),
      .fallback_color = draft_layer_config_.fallback_color(),
      .alpha_mode = draft_layer_config_.alpha_mode(),
      .alpha_coefficient = draft_layer_config_.alpha_coefficient(),
      .image_source_transformation = image_source_transformation,
  });

  draft_layer_config_differs_from_applied_ = true;
}

void Layer::SetPrimaryAlpha(display::AlphaMode alpha_mode, float alpha_coefficient) {
  draft_layer_config_ = display::DriverLayer({
      .display_destination = draft_layer_config_.display_destination(),
      .image_source = draft_layer_config_.image_source(),
      .image_id = draft_layer_config_.image_id(),
      .image_metadata = draft_layer_config_.image_metadata(),
      .fallback_color = draft_layer_config_.fallback_color(),
      .alpha_mode = alpha_mode,
      .alpha_coefficient = alpha_coefficient,
      .image_source_transformation = draft_layer_config_.image_source_transformation(),
  });

  draft_layer_config_differs_from_applied_ = true;
}

void Layer::SetColorConfig(display::Color color, display::Rectangle display_destination) {
  draft_layer_config_ = display::DriverLayer({
      .display_destination = display_destination,
      .image_source = display::Rectangle({.x = 0, .y = 0, .width = 0, .height = 0}),
      .image_id = display::kInvalidDriverImageId,
      .image_metadata = display::ImageMetadata({
          .width = 0,
          .height = 0,
          .tiling_type = display::ImageTilingType::kLinear,
      }),
      .fallback_color = color,
      .alpha_mode = draft_layer_config_.alpha_mode(),
      .alpha_coefficient = draft_layer_config_.alpha_coefficient(),
      .image_source_transformation = draft_layer_config_.image_source_transformation(),
  });

  draft_layer_config_differs_from_applied_ = true;

  draft_image_ = nullptr;
}

void Layer::SetImage(fbl::RefPtr<Image> image, display::EventId wait_event_id) {
  draft_image_ = std::move(image);
  draft_image_wait_event_id_ = wait_event_id;
}

bool Layer::MarkFenceReady(FenceReference* fence) {
  ZX_DEBUG_ASSERT(controller_.IsRunningOnDriverDispatcher());
  return waiting_images_.MarkFenceReady(fence);
}

bool Layer::HasWaitingImages() const {
  ZX_DEBUG_ASSERT(controller_.IsRunningOnDriverDispatcher());
  return waiting_images_.size() > 0;
}

void Layer::RetireDraftImage() { draft_image_ = nullptr; }

void Layer::RetireWaitingImage(const Image& image) {
  ZX_DEBUG_ASSERT(controller_.IsRunningOnDriverDispatcher());
  waiting_images_.RemoveImage(image);
}

bool Layer::RetireAppliedImage() {
  if (applied_image_ == nullptr) {
    return false;
  }
  applied_image_ = nullptr;

  return applied_display_config_list_node_.InContainer();
}

}  // namespace display_coordinator
