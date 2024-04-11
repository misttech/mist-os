// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/layer.h"

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/ddk/debug.h>
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

#include "src/graphics/display/drivers/coordinator/fence.h"
#include "src/graphics/display/drivers/coordinator/image.h"
#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types-cpp/display-id.h"
#include "src/graphics/display/lib/api-types-cpp/driver-image-id.h"
#include "src/graphics/display/lib/api-types-cpp/driver-layer-id.h"
#include "src/graphics/display/lib/api-types-cpp/event-id.h"

namespace fhdt = fuchsia_hardware_display_types;

namespace display {

namespace {

static constexpr uint32_t kInvalidLayerType = UINT32_MAX;

// Removes and invokes EarlyRetire on all entries before end.
static void EarlyRetireUpTo(Image::DoublyLinkedList& list, Image::DoublyLinkedList::iterator end) {
  while (list.begin() != end) {
    fbl::RefPtr<Image> image = list.pop_front();
    image->EarlyRetire();
  }
}

static void populate_image(const fhdt::wire::ImageMetadata& image_metadata, image_t* image_out) {
  image_out->width = image_metadata.width;
  image_out->height = image_metadata.height;
  image_out->tiling_type = image_metadata.tiling_type;
}

}  // namespace

Layer::Layer(DriverLayerId id) {
  this->id = id;
  memset(&pending_layer_, 0, sizeof(layer_t));
  memset(&current_layer_, 0, sizeof(layer_t));
  config_change_ = false;
  pending_node_.layer = this;
  current_node_.layer = this;
  current_display_id_ = kInvalidDisplayId;
  current_layer_.type = kInvalidLayerType;
  pending_layer_.type = kInvalidLayerType;
  is_skipped_ = false;
}

Layer::~Layer() {
  if (pending_image_) {
    pending_image_->DiscardAcquire();
  }
  EarlyRetireUpTo(waiting_images_, waiting_images_.end());
  if (displayed_image_) {
    fbl::AutoLock lock(displayed_image_->mtx());
    displayed_image_->StartRetire();
  }
}

bool Layer::ResolvePendingLayerProperties() {
  // If the layer's image configuration changed, get rid of any current images
  if (pending_image_config_gen_ != current_image_config_gen_) {
    current_image_config_gen_ = pending_image_config_gen_;

    if (pending_image_ == nullptr) {
      zxlogf(ERROR, "Tried to apply configuration with missing image");
      return false;
    }

    EarlyRetireUpTo(waiting_images_, waiting_images_.end());
    if (displayed_image_ != nullptr) {
      {
        fbl::AutoLock lock(displayed_image_->mtx());
        displayed_image_->StartRetire();
      }
      displayed_image_ = nullptr;
    }
  }
  return true;
}

bool Layer::ResolvePendingImage(FenceCollection* fences, ConfigStamp stamp) {
  if (pending_image_) {
    auto wait_fence = fences->GetFence(pending_wait_event_id_);
    if (wait_fence && wait_fence->InContainer()) {
      zxlogf(ERROR, "Tried to wait with a busy event");
      return false;
    }
    pending_image_->PrepareFences(std::move(wait_fence),
                                  fences->GetFence(pending_signal_event_id_));
    {
      fbl::AutoLock lock(pending_image_->mtx());
      waiting_images_.push_back(std::move(pending_image_));
    }
  }

  if (!waiting_images_.is_empty()) {
    waiting_images_.back().set_latest_client_config_stamp(stamp);
  }
  return true;
}

void Layer::ApplyChanges(const display_mode_t& mode) {
  if (!config_change_) {
    return;
  }

  current_layer_ = pending_layer_;
  config_change_ = false;

  if (current_layer_.type == LAYER_TYPE_PRIMARY) {
    if (displayed_image_) {
      current_layer_.cfg.primary.image.handle = ToBanjoDriverImageId(displayed_image_->driver_id());
    }
    return;
  }

  if (current_layer_.type == LAYER_TYPE_COLOR) {
    memcpy(current_color_bytes_, pending_color_bytes_, sizeof(current_color_bytes_));
    current_layer_.cfg.color.color_list = current_color_bytes_;
    current_layer_.cfg.color.color_count = 4;
    return;
  }

  ZX_DEBUG_ASSERT_MSG(false, "CheckConfig() failed to bounce invalid layer type %" PRIu32,
                      current_layer_.type);
}

void Layer::DiscardChanges() {
  pending_image_config_gen_ = current_image_config_gen_;
  if (pending_image_) {
    pending_image_->DiscardAcquire();
    pending_image_ = nullptr;
  }
  if (config_change_) {
    pending_layer_ = current_layer_;
    config_change_ = false;
  }

  memcpy(pending_color_bytes_, current_color_bytes_, sizeof(pending_color_bytes_));
}

bool Layer::CleanUpAllImages() {
  RetirePendingImage();

  // Retire all waiting images.
  EarlyRetireUpTo(waiting_images_, waiting_images_.end());

  return RetireDisplayedImage();
}

bool Layer::CleanUpImage(const Image& image) {
  if (pending_image_.get() == &image) {
    RetirePendingImage();
  }

  RetireWaitingImage(image);

  if (displayed_image_.get() == &image) {
    return RetireDisplayedImage();
  }
  return false;
}

std::optional<ConfigStamp> Layer::GetCurrentClientConfigStamp() const {
  if (displayed_image_ != nullptr) {
    return displayed_image_->latest_client_config_stamp();
  }
  return std::nullopt;
}

bool Layer::ActivateLatestReadyImage() {
  if (waiting_images_.is_empty()) {
    return false;
  }

  // Find the most recent (i.e. the most behind) waiting image that is ready.
  auto it = waiting_images_.end();
  bool found_ready_image = false;
  do {
    --it;
    if (it->IsReady()) {
      found_ready_image = true;
      break;
    }
  } while (it != waiting_images_.begin());

  if (!found_ready_image) {
    return false;
  }

  // Retire the last active image
  if (displayed_image_ != nullptr) {
    fbl::AutoLock lock(displayed_image_->mtx());
    displayed_image_->StartRetire();
  }

  // Retire the waiting images that were never presented.
  EarlyRetireUpTo(waiting_images_, /*end=*/it);
  displayed_image_ = waiting_images_.pop_front();

  if (current_layer_.type == LAYER_TYPE_PRIMARY) {
    uint64_t handle = ToBanjoDriverImageId(displayed_image_->driver_id());
    current_layer_.cfg.primary.image.handle = handle;
  } else {
    // type is validated in Client::CheckConfig, so something must be very wrong.
    ZX_ASSERT(false);
  }
  return true;
}

bool Layer::AddToConfig(fbl::DoublyLinkedList<LayerNode*>* list, uint32_t z_index) {
  if (pending_node_.InContainer()) {
    return false;
  } else {
    pending_layer_.z_index = z_index;
    list->push_front(&pending_node_);
    return true;
  }
}

void Layer::SetPrimaryConfig(fhdt::wire::ImageMetadata image_metadata) {
  pending_layer_.type = LAYER_TYPE_PRIMARY;
  auto* primary = &pending_layer_.cfg.primary;
  populate_image(image_metadata, &primary->image);
  const frame_t new_frame = {
      .x_pos = 0, .y_pos = 0, .width = image_metadata.width, .height = image_metadata.height};
  primary->src_frame = new_frame;
  primary->dest_frame = new_frame;
  pending_image_config_gen_++;
  pending_image_ = nullptr;
  config_change_ = true;
}

void Layer::SetPrimaryPosition(fhdt::wire::Transform transform, fhdt::wire::Frame src_frame,
                               fhdt::wire::Frame dest_frame) {
  primary_layer_t* primary_layer = &pending_layer_.cfg.primary;

  static_assert(sizeof(fhdt::wire::Frame) == sizeof(frame_t), "Struct mismatch");
  static_assert(offsetof(fhdt::wire::Frame, x_pos) == offsetof(frame_t, x_pos), "Struct mismatch");
  static_assert(offsetof(fhdt::wire::Frame, y_pos) == offsetof(frame_t, y_pos), "Struct mismatch");
  static_assert(offsetof(fhdt::wire::Frame, width) == offsetof(frame_t, width), "Struct mismatch");
  static_assert(offsetof(fhdt::wire::Frame, height) == offsetof(frame_t, height),
                "Struct mismatch");

  memcpy(&primary_layer->src_frame, &src_frame, sizeof(frame_t));
  memcpy(&primary_layer->dest_frame, &dest_frame, sizeof(frame_t));
  primary_layer->transform_mode = static_cast<uint8_t>(transform);

  config_change_ = true;
}

void Layer::SetPrimaryAlpha(fhdt::wire::AlphaMode mode, float val) {
  primary_layer_t* primary_layer = &pending_layer_.cfg.primary;

  static_assert(static_cast<alpha_t>(fhdt::wire::AlphaMode::kDisable) == ALPHA_DISABLE,
                "Bad constant");
  static_assert(static_cast<alpha_t>(fhdt::wire::AlphaMode::kPremultiplied) == ALPHA_PREMULTIPLIED,
                "Bad constant");
  static_assert(static_cast<alpha_t>(fhdt::wire::AlphaMode::kHwMultiply) == ALPHA_HW_MULTIPLY,
                "Bad constant");

  primary_layer->alpha_mode = static_cast<alpha_t>(mode);
  primary_layer->alpha_layer_val = val;

  config_change_ = true;
}

void Layer::SetColorConfig(fuchsia_images2::wire::PixelFormat pixel_format,
                           ::fidl::VectorView<uint8_t> color_bytes) {
  // Increase the size of the static array when large color formats are introduced
  ZX_ASSERT(color_bytes.count() <= sizeof(pending_color_bytes_));

  pending_layer_.type = LAYER_TYPE_COLOR;
  color_layer_t* color_layer = &pending_layer_.cfg.color;

  ZX_DEBUG_ASSERT(!pixel_format.IsUnknown());
  color_layer->format = static_cast<fuchsia_images2_pixel_format_enum_value_t>(pixel_format);
  memcpy(pending_color_bytes_, color_bytes.data(), sizeof(pending_color_bytes_));

  pending_image_ = nullptr;
  config_change_ = true;
}

void Layer::SetImage(fbl::RefPtr<Image> image, EventId wait_event_id, EventId signal_event_id) {
  if (pending_image_) {
    pending_image_->DiscardAcquire();
  }

  pending_image_ = image;
  pending_wait_event_id_ = wait_event_id;
  pending_signal_event_id_ = signal_event_id;
}

void Layer::RetirePendingImage() {
  if (pending_image_) {
    pending_image_->DiscardAcquire();
    pending_image_ = nullptr;
  }
}

void Layer::RetireWaitingImage(const Image& image) {
  auto it = waiting_images_.find_if([&image](const Image& node) { return &node == &image; });
  if (it != waiting_images_.end()) {
    fbl::RefPtr<Image> to_retire = waiting_images_.erase(it);
    ZX_DEBUG_ASSERT(to_retire);
    to_retire->EarlyRetire();
  }
}

bool Layer::RetireDisplayedImage() {
  if (!displayed_image_) {
    return false;
  }

  {
    fbl::AutoLock lock(displayed_image_->mtx());
    displayed_image_->StartRetire();
  }
  displayed_image_ = nullptr;

  return current_node_.InContainer();
}

}  // namespace display
