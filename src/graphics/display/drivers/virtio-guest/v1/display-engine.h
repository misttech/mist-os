// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_DISPLAY_ENGINE_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_DISPLAY_ENGINE_H_

#include <fidl/fuchsia.hardware.sysmem/cpp/wire.h>
#include <fidl/fuchsia.images2/cpp/wire.h>
#include <fidl/fuchsia.sysmem/cpp/wire.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/ddk/device.h>
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

#include <ddktl/device.h>
#include <fbl/condition_variable.h>
#include <fbl/mutex.h>

#include "src/graphics/display/drivers/virtio-guest/v1/virtio-gpu-device.h"
#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types-cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types-cpp/driver-image-id.h"
#include "src/graphics/lib/virtio/virtio-abi.h"

namespace virtio_display {

class Ring;

class DisplayEngine : public ddk::DisplayControllerImplProtocol<DisplayEngine, ddk::base_protocol> {
 public:
  struct BufferInfo {
    zx::vmo vmo = {};
    size_t offset = 0;
    uint32_t bytes_per_pixel = 0;
    uint32_t bytes_per_row = 0;
    fuchsia_images2::wire::PixelFormat pixel_format;
  };

  static zx::result<std::unique_ptr<DisplayEngine>> Create(zx_device_t* bus_device);

  // Exposed for testing. Production code must use the Create() factory method.
  DisplayEngine(zx_device_t* bus_device, fidl::ClientEnd<fuchsia_sysmem::Allocator> sysmem_client,
                std::unique_ptr<VirtioGpuDevice> gpu_device);
  ~DisplayEngine();

  zx_status_t Init();
  zx_status_t DdkGetProtocol(uint32_t proto_id, void* out);
  zx_status_t Start();

  // Finds the first display usable by this driver, in the `display_infos` list.
  //
  // Returns nullptr if the list does not contain a usable display.
  const DisplayInfo* FirstValidDisplay(cpp20::span<const DisplayInfo> display_infos);

  const virtio_abi::ScanoutInfo* pmode() const { return &current_display_.scanout_info; }

  zx::result<BufferInfo> GetAllocatedBufferInfoForImage(
      display::DriverBufferCollectionId driver_buffer_collection_id, uint32_t index,
      const image_metadata_t& image_metadata) const;

  void DisplayControllerImplSetDisplayControllerInterface(
      const display_controller_interface_protocol_t* intf);
  void DisplayControllerImplResetDisplayControllerInterface();

  zx_status_t DisplayControllerImplImportBufferCollection(
      uint64_t banjo_driver_buffer_collection_id, zx::channel collection_token);
  zx_status_t DisplayControllerImplReleaseBufferCollection(
      uint64_t banjo_driver_buffer_collection_id);

  zx_status_t DisplayControllerImplImportImage(const image_metadata_t* image_metadata,
                                               uint64_t banjo_driver_buffer_collection_id,
                                               uint32_t index, uint64_t* out_image_handle);

  zx_status_t DisplayControllerImplImportImageForCapture(uint64_t banjo_driver_buffer_collection_id,
                                                         uint32_t index,
                                                         uint64_t* out_capture_handle) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  void DisplayControllerImplReleaseImage(uint64_t image_handle);

  config_check_result_t DisplayControllerImplCheckConfiguration(
      const display_config_t** display_configs, size_t display_count,
      client_composition_opcode_t* out_client_composition_opcodes_list,
      size_t client_composition_opcodes_count, size_t* out_client_composition_opcodes_actual);

  void DisplayControllerImplApplyConfiguration(const display_config_t** display_configs,
                                               size_t display_count,
                                               const config_stamp_t* banjo_config_stamp);

  void DisplayControllerImplSetEld(uint64_t display_id, const uint8_t* raw_eld_list,
                                   size_t raw_eld_count) {}  // No ELD required for non-HDA systems.
  zx_status_t DisplayControllerImplGetSysmemConnection(zx::channel sysmem_handle);

  zx_status_t DisplayControllerImplSetBufferCollectionConstraints(
      const image_buffer_usage_t* usage, uint64_t banjo_driver_buffer_collection_id);
  zx_status_t DisplayControllerImplSetDisplayPower(uint64_t display_id, bool power_on);

  bool DisplayControllerImplIsCaptureSupported() { return false; }

  zx_status_t DisplayControllerImplStartCapture(uint64_t capture_handle) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  zx_status_t DisplayControllerImplReleaseCapture(uint64_t capture_handle) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  bool DisplayControllerImplIsCaptureCompleted() { return false; }

  zx_status_t DisplayControllerImplSetMinimumRgb(uint8_t minimum_rgb) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  VirtioPciDevice& pci_device() { return gpu_device_->pci_device(); }

 private:
  // TODO(https://fxbug.dev/42073721): Support more formats.
  static constexpr std::array<fuchsia_images2_pixel_format_enum_value_t, 1> kSupportedFormats = {
      static_cast<fuchsia_images2_pixel_format_enum_value_t>(
          fuchsia_images2::wire::PixelFormat::kB8G8R8A8),
  };

  zx::result<display::DriverImageId> Import(zx::vmo vmo, const image_metadata& image_metadata,
                                            size_t offset, uint32_t pixel_size, uint32_t row_bytes,
                                            fuchsia_images2::wire::PixelFormat pixel_format);

  // Initializes the sysmem Allocator client used to import incoming buffer
  // collection tokens.
  //
  // On success, returns ZX_OK and the sysmem allocator client will be open
  // until the device is released.
  zx_status_t InitSysmemAllocatorClient();

  // gpu op
  io_buffer_t gpu_req_ = {};

  DisplayInfo current_display_;

  // Flush thread
  void virtio_gpu_flusher();
  thrd_t flush_thread_ = {};
  fbl::Mutex flush_lock_;

  display_controller_interface_protocol_t dc_intf_ = {};
  // The sysmem allocator client used to bind incoming buffer collection tokens.
  fidl::WireSyncClient<fuchsia_sysmem::Allocator> sysmem_;

  zx_device_t* const bus_device_;

  // Imported sysmem buffer collections.
  std::unordered_map<display::DriverBufferCollectionId,
                     fidl::WireSyncClient<fuchsia_sysmem::BufferCollection>>
      buffer_collections_;

  struct imported_image* latest_fb_ = nullptr;
  struct imported_image* displayed_fb_ = nullptr;
  display::ConfigStamp latest_config_stamp_ = display::kInvalidConfigStamp;
  display::ConfigStamp displayed_config_stamp_ = display::kInvalidConfigStamp;

  std::unique_ptr<VirtioGpuDevice> gpu_device_;
};

}  // namespace virtio_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_DISPLAY_ENGINE_H_
