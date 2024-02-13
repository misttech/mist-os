// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_GPU_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_GPU_H_

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
#include <optional>

#include <ddktl/device.h>
#include <fbl/condition_variable.h>
#include <fbl/mutex.h>

#include "src/graphics/display/drivers/virtio-guest/v1/virtio-abi.h"
#include "src/graphics/display/drivers/virtio-guest/v1/virtio-pci-device.h"
#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types-cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types-cpp/driver-image-id.h"

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
                std::unique_ptr<VirtioPciDevice> virtio_device);
  ~DisplayEngine();

  zx_status_t Init();
  zx_status_t DdkGetProtocol(uint32_t proto_id, void* out);
  zx_status_t Start();

  const virtio_abi::ScanoutInfo* pmode() const { return &pmode_; }

  zx::result<BufferInfo> GetAllocatedBufferInfoForImage(
      display::DriverBufferCollectionId driver_buffer_collection_id, uint32_t index,
      const image_t* image) const;

  void DisplayControllerImplSetDisplayControllerInterface(
      const display_controller_interface_protocol_t* intf);

  zx_status_t DisplayControllerImplSetDisplayCaptureInterface(
      const display_capture_interface_protocol_t* intf) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  zx_status_t DisplayControllerImplImportBufferCollection(
      uint64_t banjo_driver_buffer_collection_id, zx::channel collection_token);
  zx_status_t DisplayControllerImplReleaseBufferCollection(
      uint64_t banjo_driver_buffer_collection_id);

  zx_status_t DisplayControllerImplImportImage(const image_t* image,
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
      const image_t* config, uint64_t banjo_driver_buffer_collection_id);
  zx_status_t DisplayControllerImplSetDisplayPower(uint64_t display_id, bool power_on);

  zx_status_t DisplayControllerImplStartCapture(uint64_t capture_handle) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  zx_status_t DisplayControllerImplReleaseCapture(uint64_t capture_handle) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  bool DisplayControllerImplIsCaptureCompleted() { return false; }

 private:
  // TODO(https://fxbug.dev/42073721): Support more formats.
  static constexpr std::array<fuchsia_images2_pixel_format_enum_value_t, 1> kSupportedFormats = {
      static_cast<fuchsia_images2_pixel_format_enum_value_t>(
          fuchsia_images2::wire::PixelFormat::kB8G8R8A8),
  };

  zx::result<display::DriverImageId> Import(zx::vmo vmo, const image_t* image, size_t offset,
                                            uint32_t pixel_size, uint32_t row_bytes,
                                            fuchsia_images2::wire::PixelFormat pixel_format);

  // Retrieves the current output configuration.
  //
  // virtio spec Section 5.7.6.8 "Device Operation: controlq", operation
  // VIRTIO_GPU_CMD_GET_DISPLAY_INFO.
  zx_status_t GetDisplayInfo();

  // Creates a 2D resource on the virtio host.
  //
  // The resource ID is automatically allocated.
  //
  // virtio spec Section 5.7.6.8 "Device Operation: controlq", operation
  // VIRTIO_GPU_CMD_RESOURCE_CREATE_2D.
  zx_status_t Create2DResource(uint32_t* resource_id, uint32_t width, uint32_t height,
                               fuchsia_images2::wire::PixelFormat pixel_format);

  // Sets scanout parameters for one scanout.
  //
  // virtio spec Section 5.7.6.8 "Device Operation: controlq", operation
  // VIRTIO_GPU_CMD_SET_SCANOUT.
  zx_status_t SetScanoutProperties(uint32_t scanout_id, uint32_t resource_id, uint32_t width,
                                   uint32_t height);

  // Flushes any scanouts that use `resource_id` to the host screen.
  //
  // virtio spec Section 5.7.6.8 "Device Operation: controlq", operation
  // VIRTIO_GPU_CMD_RESOURCE_FLUSH.
  zx_status_t FlushResource(uint32_t resource_id, uint32_t width, uint32_t height);

  // Transfers data from a guest resource to host memory.
  //
  // virtio spec Section 5.7.6.8 "Device Operation: controlq", operation
  // VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D.
  zx_status_t TransferToHost2D(uint32_t resource_id, uint32_t width, uint32_t height);

  // Assigns an array of guest pages as the backing store for a resource.
  //
  // virtio spec Section 5.7.6.8 "Device Operation: controlq", operation
  // VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING.
  zx_status_t AttachResourceBacking(uint32_t resource_id, zx_paddr_t ptr, size_t buf_len);

  // Initializes the sysmem Allocator client used to import incoming buffer
  // collection tokens.
  //
  // On success, returns ZX_OK and the sysmem allocator client will be open
  // until the device is released.
  zx_status_t InitSysmemAllocatorClient();

  // gpu op
  io_buffer_t gpu_req_ = {};

  // A saved copy of the display
  virtio_abi::ScanoutInfo pmode_ = {};
  int pmode_id_ = -1;

  uint32_t next_resource_id_ = 1;

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

  std::unique_ptr<VirtioPciDevice> virtio_device_;
};

}  // namespace virtio_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_GPU_H_
