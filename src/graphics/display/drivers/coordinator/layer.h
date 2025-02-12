// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_LAYER_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_LAYER_H_

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fidl/fuchsia.images2/cpp/wire.h>
#include <fidl/fuchsia.math/cpp/wire.h>
#include <zircon/types.h>

#include <memory>

#include <fbl/intrusive_double_list.h>
#include <fbl/ref_ptr.h>
#include <fbl/slab_allocator.h>

#include "src/graphics/display/drivers/coordinator/fence.h"
#include "src/graphics/display/drivers/coordinator/id-map.h"
#include "src/graphics/display/drivers/coordinator/image.h"
#include "src/graphics/display/drivers/coordinator/waiting-image-list.h"
#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-layer-id.h"
#include "src/graphics/display/lib/api-types/cpp/event-id.h"

namespace display_coordinator {

class FenceCollection;
class Layer;
class LayerTest;
class Client;

struct LayerNode : public fbl::DoublyLinkedListable<LayerNode*> {
  Layer* layer;
};

// Almost-POD used by Client to manage layer state. Public state is used by Controller.
class Layer : public IdMappable<std::unique_ptr<Layer>, display::DriverLayerId> {
 public:
  // `controller` must be non-null.
  explicit Layer(Controller* controller, display::DriverLayerId id);
  ~Layer();

  fbl::RefPtr<Image> current_image() const { return displayed_image_; }
  bool is_skipped() const { return is_skipped_; }

  // TODO(https://fxbug.dev/42118906) Although this is nominally a POD, the state management and
  // lifecycle are complicated by interactions with Client's threading model.
  friend Client;
  friend LayerTest;

  bool in_use() const { return current_node_.InContainer() || pending_node_.InContainer(); }

  const image_metadata_t& pending_image_metadata() const { return pending_layer_.image_metadata; }
  uint64_t pending_image_handle() const { return pending_layer_.image_handle; }

  // If the layer properties were changed in the pending configuration, this
  // retires all images as they are invalidated with layer properties change.
  bool ResolvePendingLayerProperties();

  // This sets up the fence and config stamp for pending images on this layer.
  //
  // - If the layer image has a fence to wait before presentation, this prepares
  //   the new fence and start async waiting for the fence.
  // - The layer's latest pending (waiting) image will be associated with the
  //   client configuration |stamp|, as it reflects the latest configuration
  //   state; this will overwrite all the previous stamp states for this image.
  //   The stamp will be used later when display core integrates stamps of all
  //   layers to determine the current frame state.
  //
  // Returns false if there were any errors.
  bool ResolvePendingImage(FenceCollection* fence,
                           display::ConfigStamp stamp = display::kInvalidConfigStamp);

  // Make the staged config current.
  void ApplyChanges(const display_mode_t& mode);

  // Discard the pending changes
  void DiscardChanges();

  // Removes references to all Images associated with this Layer.
  // Returns true if the current config has been affected.
  bool CleanUpAllImages() __TA_REQUIRES(mtx());

  // Removes references to the provided Image. `image` must be valid.
  // Returns true if the current config has been affected.
  bool CleanUpImage(const Image& image) __TA_REQUIRES(mtx());

  // If a new image is available, retire current_image() and other pending images. Returns false if
  // no images were ready.
  bool ActivateLatestReadyImage();

  // Get the stamp of configuration that is associated (at ResolvePendingImage)
  // with the image that is currently being displayed on the device.
  // If no image is being displayed on this layer, returns nullopt.
  std::optional<display::ConfigStamp> GetCurrentClientConfigStamp() const;

  // Adds the pending_layer_ to the end of a display list.
  //
  // Returns false if the pending_layer_ is currently in use.
  bool AppendToConfig(fbl::DoublyLinkedList<LayerNode*>* list);

  void SetPrimaryConfig(fuchsia_hardware_display_types::wire::ImageMetadata image_metadata);
  void SetPrimaryPosition(
      fuchsia_hardware_display_types::wire::CoordinateTransformation image_source_transformation,
      fuchsia_math::wire::RectU image_source, fuchsia_math::wire::RectU display_destination);
  void SetPrimaryAlpha(fuchsia_hardware_display_types::wire::AlphaMode mode, float val);
  void SetColorConfig(fuchsia_hardware_display_types::wire::Color color);
  void SetImage(fbl::RefPtr<Image> image_id, display::EventId wait_event_id);

  // Called on all waiting images when any fence fires. Returns true if an image is ready to
  // present.
  bool MarkFenceReady(FenceReference* fence);

  // Returns true if the layer has any waiting images. An image transitions from "pending" to
  // "waiting" (in the context of a specific layer) when that layer appears in an applied config.
  bool HasWaitingImages() const;

  // Aliases controller_.mtx() for the purpose of thread-safety analysis.
  fbl::Mutex* mtx() const;

 private:
  // Retires the `pending_image_`.
  void RetirePendingImage();

  // Retires the `image` from the `waiting_images_` list.
  // Does nothing if `image` is not in the list.
  void RetireWaitingImage(const Image& image);

  // Retires the image that is being displayed.
  // Returns true if this affects the current display config.
  bool RetireDisplayedImage() __TA_REQUIRES(mtx());

  Controller& controller_;

  layer_t pending_layer_;
  layer_t current_layer_;
  // flag indicating that there are changes in pending_layer that
  // need to be applied to current_layer.
  bool config_change_;

  // The event passed to SetLayerImage which hasn't been applied yet.
  display::EventId pending_wait_event_id_ = display::kInvalidEventId;

  // The image given to SetLayerImage which hasn't been applied yet.
  fbl::RefPtr<Image> pending_image_;

  // Manages images which are waiting to be displayed. Each one has an optional wait fence which
  // may not have been signaled yet.
  //
  // Must be accessed on `controller_`'s client dispatcher loop. Call-sites must guarantee this via
  // `ZX_DEBUG_ASSERT(controller_.IsRunningOnClientDispatcher())`.
  WaitingImageList waiting_images_;

  // The image which has most recently been sent to the display controller impl
  fbl::RefPtr<Image> displayed_image_;

  // Counters used for keeping track of when the layer's images need to be dropped.
  uint64_t pending_image_config_gen_ = 0;
  uint64_t current_image_config_gen_ = 0;

  LayerNode pending_node_;
  LayerNode current_node_;

  // The display this layer was most recently displayed on
  display::DisplayId current_display_id_;

  bool is_skipped_;
};

}  // namespace display_coordinator

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_LAYER_H_
