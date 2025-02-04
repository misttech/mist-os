// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GPU_DISPLAY_DISPLAY_ENGINE_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GPU_DISPLAY_DISPLAY_ENGINE_H_

#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <lib/stdcompat/span.h>
#include <lib/virtio/backends/backend.h>
#include <lib/zx/bti.h>
#include <lib/zx/result.h>
#include <lib/zx/vmo.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <cstdint>
#include <memory>

#include <fbl/condition_variable.h>
#include <fbl/mutex.h>

#include "src/graphics/display/drivers/virtio-gpu-display/imported-images.h"
#include "src/graphics/display/drivers/virtio-gpu-display/virtio-gpu-device.h"
#include "src/graphics/display/lib/api-protocols/cpp/display-engine-events-interface.h"
#include "src/graphics/display/lib/api-protocols/cpp/display-engine-interface.h"
#include "src/graphics/display/lib/api-types/cpp/config-check-result.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-capture-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/driver-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-layer.h"
#include "src/graphics/display/lib/api-types/cpp/image-buffer-usage.h"
#include "src/graphics/display/lib/api-types/cpp/image-metadata.h"
#include "src/graphics/display/lib/api-types/cpp/layer-composition-operations.h"
#include "src/graphics/display/lib/api-types/cpp/mode-id.h"
#include "src/graphics/lib/virtio/virtio-abi.h"

namespace virtio_display {

class Ring;

class DisplayEngine final : public display::DisplayEngineInterface {
 public:
  static zx::result<std::unique_ptr<DisplayEngine>> Create(
      fidl::ClientEnd<fuchsia_sysmem2::Allocator> sysmem_client, zx::bti bti,
      std::unique_ptr<virtio::Backend> backend,
      display::DisplayEngineEventsInterface* engine_events);

  // Exposed for testing. Production code must use the Create() factory method.
  //
  // `engine_events` must not be null, and must outlive the newly created
  // instance. `gpu_device` must not be null.
  DisplayEngine(display::DisplayEngineEventsInterface* engine_events,
                fidl::ClientEnd<fuchsia_sysmem2::Allocator> sysmem_client,
                std::unique_ptr<VirtioGpuDevice> gpu_device);
  ~DisplayEngine();

  zx_status_t Init();
  zx_status_t Start();

  // DisplayEngineInterface:
  void OnCoordinatorConnected() override;
  zx::result<> ImportBufferCollection(
      display::DriverBufferCollectionId buffer_collection_id,
      fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> buffer_collection_token) override;
  zx::result<> ReleaseBufferCollection(
      display::DriverBufferCollectionId buffer_collection_id) override;
  zx::result<display::DriverImageId> ImportImage(
      const display::ImageMetadata& image_metadata,
      display::DriverBufferCollectionId buffer_collection_id, uint32_t buffer_index) override;
  zx::result<display::DriverCaptureImageId> ImportImageForCapture(
      display::DriverBufferCollectionId buffer_collection_id, uint32_t buffer_index) override;
  void ReleaseImage(display::DriverImageId image_id) override;
  display::ConfigCheckResult CheckConfiguration(
      display::DisplayId display_id, display::ModeId display_mode_id,
      cpp20::span<const display::DriverLayer> layers,
      cpp20::span<display::LayerCompositionOperations> layer_composition_operations) override;
  void ApplyConfiguration(display::DisplayId display_id, display::ModeId display_mode_id,
                          cpp20::span<const display::DriverLayer> layers,
                          display::DriverConfigStamp config_stamp) override;
  zx::result<> SetBufferCollectionConstraints(
      const display::ImageBufferUsage& image_buffer_usage,
      display::DriverBufferCollectionId buffer_collection_id) override;
  zx::result<> SetDisplayPower(display::DisplayId display_id, bool power_on) override;
  bool IsCaptureSupported() override;
  zx::result<> StartCapture(display::DriverCaptureImageId capture_image_id) override;
  zx::result<> ReleaseCapture(display::DriverCaptureImageId capture_image_id) override;
  zx::result<> SetMinimumRgb(uint8_t minimum_rgb) override;

  // Finds the first display usable by this driver, in the `display_infos` list.
  //
  // Returns nullptr if the list does not contain a usable display.
  const DisplayInfo* FirstValidDisplay(cpp20::span<const DisplayInfo> display_infos);

  const virtio_abi::ScanoutInfo* pmode() const { return &current_display_.scanout_info; }

  VirtioPciDevice& pci_device() { return gpu_device_->pci_device(); }

  ImportedImages* imported_images_for_testing() { return &imported_images_; }

 private:
  DisplayInfo current_display_;

  // Flush thread
  void virtio_gpu_flusher();
  thrd_t flush_thread_ = {};
  fbl::Mutex flush_lock_;

  // The sysmem allocator client used to bind incoming buffer collection tokens.
  ImportedImages imported_images_;

  display::DisplayEngineEventsInterface& engine_events_;

  uint32_t latest_framebuffer_resource_id_ = virtio_abi::kInvalidResourceId;
  uint32_t displayed_framebuffer_resource_id_ = virtio_abi::kInvalidResourceId;
  display::DriverConfigStamp latest_config_stamp_ = display::kInvalidDriverConfigStamp;
  display::DriverConfigStamp displayed_config_stamp_ = display::kInvalidDriverConfigStamp;

  std::unique_ptr<VirtioGpuDevice> gpu_device_;
};

}  // namespace virtio_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GPU_DISPLAY_DISPLAY_ENGINE_H_
