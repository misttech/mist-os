// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GPU_DISPLAY_DISPLAY_ENGINE_INTERFACE_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GPU_DISPLAY_DISPLAY_ENGINE_INTERFACE_H_

#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <lib/zx/result.h>

#include <cstdint>

#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-capture-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-layer.h"
#include "src/graphics/display/lib/api-types/cpp/image-buffer-usage.h"
#include "src/graphics/display/lib/api-types/cpp/image-metadata.h"
#include "src/graphics/display/lib/api-types/cpp/layer-composition-operations.h"

namespace virtio_display {

// The methods in the [`fuchsia.hardware.display.engine/Engine`] FIDL interface.
//
// This abstract base class only represents the methods in the FIDL interface.
// The events are represented by `DisplayEngineEventsInterface`.
//
// This abstract base class also represents the
// [`fuchsia.hardware.display.controller/DisplayEngine`] Banjo
// interface.
class DisplayEngineInterface {
 public:
  DisplayEngineInterface() = default;

  DisplayEngineInterface(const DisplayEngineInterface&) = delete;
  DisplayEngineInterface(DisplayEngineInterface&&) = delete;
  DisplayEngineInterface& operator=(const DisplayEngineInterface&) = delete;
  DisplayEngineInterface& operator=(DisplayEngineInterface&&) = delete;

  virtual void OnCoordinatorConnected() = 0;

  virtual zx::result<> ImportBufferCollection(
      display::DriverBufferCollectionId buffer_collection_id,
      fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> buffer_collection_token) = 0;
  virtual zx::result<> ReleaseBufferCollection(
      display::DriverBufferCollectionId buffer_collection_id) = 0;

  virtual zx::result<display::DriverImageId> ImportImage(
      const display::ImageMetadata& image_metadata,
      display::DriverBufferCollectionId buffer_collection_id, uint32_t buffer_index) = 0;
  virtual zx::result<display::DriverCaptureImageId> ImportImageForCapture(
      display::DriverBufferCollectionId buffer_collection_id, uint32_t buffer_index) = 0;
  virtual void ReleaseImage(display::DriverImageId driver_image_id) = 0;

  // `layer_composition_operations_buffer` must have the same size as `layers`. All
  // the elements must be empty (`display::LayerCompositionOperations::kNoOperations`).
  //
  // Returns true if the configuration is acceptable to the driver. If false,
  // `layer_composition_operations` must be set to a sequence of operations that
  // are likely to yield an acceptable configuration.
  virtual bool CheckConfiguration(
      display::DisplayId display_id, cpp20::span<const display::DriverLayer> layers,
      cpp20::span<display::LayerCompositionOperations> layer_composition_operations) = 0;

  virtual void ApplyConfiguration(display::DisplayId display_id,
                                  cpp20::span<const display::DriverLayer> layers,
                                  display::ConfigStamp config_stamp) = 0;

  virtual zx::result<> SetBufferCollectionConstraints(
      const display::ImageBufferUsage& image_buffer_usage,
      display::DriverBufferCollectionId buffer_collection_id) = 0;

  virtual zx::result<> SetDisplayPower(display::DisplayId display_id, bool power_on) = 0;

  virtual bool IsCaptureSupported() = 0;
  virtual zx::result<> StartCapture(display::DriverCaptureImageId capture_image_id) = 0;
  virtual zx::result<> ReleaseCapture(display::DriverCaptureImageId capture_image_id) = 0;

  virtual zx::result<> SetMinimumRgb(uint8_t minimum_rgb) = 0;

 protected:
  // Destruction via base class pointer is not supported intentionally.
  // Instances are not expected to be owned by pointers to base classes.
  ~DisplayEngineInterface() = default;
};

}  // namespace virtio_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GPU_DISPLAY_DISPLAY_ENGINE_INTERFACE_H_
