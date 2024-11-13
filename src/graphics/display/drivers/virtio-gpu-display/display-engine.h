// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GPU_DISPLAY_DISPLAY_ENGINE_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GPU_DISPLAY_DISPLAY_ENGINE_H_

#include <fidl/fuchsia.hardware.display.engine/cpp/wire.h>
#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fidl/fuchsia.hardware.sysmem/cpp/wire.h>
#include <fidl/fuchsia.images2/cpp/wire.h>
#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/stdcompat/span.h>
#include <lib/virtio/device.h>
#include <lib/virtio/ring.h>
#include <lib/zx/pmt.h>
#include <lib/zx/result.h>
#include <lib/zx/vmo.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <cstdint>
#include <memory>

#include <fbl/condition_variable.h>
#include <fbl/mutex.h>

#include "src/graphics/display/drivers/virtio-gpu-display/display-coordinator-events-interface.h"
#include "src/graphics/display/drivers/virtio-gpu-display/display-engine-interface.h"
#include "src/graphics/display/drivers/virtio-gpu-display/virtio-gpu-device.h"
#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-capture-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/image-buffer-usage.h"
#include "src/graphics/display/lib/api-types/cpp/image-metadata.h"
#include "src/graphics/lib/virtio/virtio-abi.h"

namespace virtio_display {

class Ring;

class DisplayEngine final : public DisplayEngineInterface {
 public:
  struct BufferInfo {
    zx::vmo vmo = {};
    size_t offset = 0;
    uint32_t bytes_per_pixel = 0;
    uint32_t bytes_per_row = 0;
    fuchsia_images2::wire::PixelFormat pixel_format;
  };

  static zx::result<std::unique_ptr<DisplayEngine>> Create(
      fidl::ClientEnd<fuchsia_sysmem2::Allocator> sysmem_client, zx::bti bti,
      std::unique_ptr<virtio::Backend> backend,
      DisplayCoordinatorEventsInterface* coordinator_events);

  // Exposed for testing. Production code must use the Create() factory method.
  //
  // `bus_device` and `coordinator_events` must not be null, and must outlive
  // the newly created instance. `gpu_device` must not be null.
  DisplayEngine(DisplayCoordinatorEventsInterface* coordinator_events,
                fidl::ClientEnd<fuchsia_sysmem2::Allocator> sysmem_client,
                std::unique_ptr<VirtioGpuDevice> gpu_device);
  ~DisplayEngine();

  zx_status_t Init();
  zx_status_t Start();

  // DisplayEngineInterface:
  void OnCoordinatorConnected() override;
  zx::result<> ImportBufferCollection(
      display::DriverBufferCollectionId driver_buffer_collection_id,
      fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> buffer_collection_token) override;
  zx::result<> ReleaseBufferCollection(
      display::DriverBufferCollectionId driver_buffer_collection_id) override;
  zx::result<display::DriverImageId> ImportImage(
      const display::ImageMetadata& image_metadata,
      display::DriverBufferCollectionId driver_buffer_collection_id, uint32_t index) override;
  zx::result<display::DriverCaptureImageId> ImportImageForCapture(
      display::DriverBufferCollectionId driver_buffer_collection_id, uint32_t index) override;
  void ReleaseImage(display::DriverImageId driver_image_id) override;
  config_check_result_t CheckConfiguration(
      cpp20::span<const display_config_t> display_configs,
      cpp20::span<client_composition_opcode_t> out_client_composition_opcodes,
      size_t* out_client_composition_opcodes_actual) override;
  void ApplyConfiguration(cpp20::span<const display_config_t> display_configs,
                          const config_stamp_t* banjo_config_stamp) override;
  zx::result<> SetBufferCollectionConstraints(
      const display::ImageBufferUsage& image_buffer_usage,
      display::DriverBufferCollectionId driver_buffer_collection_id) override;
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

  zx::result<BufferInfo> GetAllocatedBufferInfoForImage(
      display::DriverBufferCollectionId driver_buffer_collection_id, uint32_t index,
      const display::ImageMetadata& image_metadata) const;

  VirtioPciDevice& pci_device() { return gpu_device_->pci_device(); }

 private:
  // TODO(https://fxbug.dev/42073721): Support more formats.
  static constexpr std::array<fuchsia_images2_pixel_format_enum_value_t, 1> kSupportedFormats = {
      static_cast<fuchsia_images2_pixel_format_enum_value_t>(
          fuchsia_images2::wire::PixelFormat::kB8G8R8A8),
  };

  zx::result<display::DriverImageId> Import(zx::vmo vmo,
                                            const display::ImageMetadata& image_metadata,
                                            size_t offset, uint32_t pixel_size, uint32_t row_bytes,
                                            fuchsia_images2::wire::PixelFormat pixel_format);

  // Initializes the sysmem Allocator client used to import incoming buffer
  // collection tokens.
  //
  // On success, returns ZX_OK and the sysmem allocator client will be open
  // until the device is released.
  zx_status_t InitSysmemAllocatorClient();

  DisplayInfo current_display_;

  // Flush thread
  void virtio_gpu_flusher();
  thrd_t flush_thread_ = {};
  fbl::Mutex flush_lock_;

  // The sysmem allocator client used to bind incoming buffer collection tokens.
  fidl::WireSyncClient<fuchsia_sysmem2::Allocator> sysmem_;

  DisplayCoordinatorEventsInterface& coordinator_events_;

  // Imported sysmem buffer collections.
  std::unordered_map<display::DriverBufferCollectionId,
                     fidl::WireSyncClient<fuchsia_sysmem2::BufferCollection>>
      buffer_collections_;

  struct imported_image* latest_fb_ = nullptr;
  struct imported_image* displayed_fb_ = nullptr;
  display::ConfigStamp latest_config_stamp_ = display::kInvalidConfigStamp;
  display::ConfigStamp displayed_config_stamp_ = display::kInvalidConfigStamp;

  std::unique_ptr<VirtioGpuDevice> gpu_device_;
};

}  // namespace virtio_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GPU_DISPLAY_DISPLAY_ENGINE_H_
