// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DRIVERS_VIRTIO_GPU_H_
#define SRC_GRAPHICS_DRIVERS_VIRTIO_GPU_H_

#include <fidl/fuchsia.hardware.sysmem/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/virtio/device.h>
#include <lib/virtio/ring.h>
#include <semaphore.h>
#include <stdlib.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>
#include <zircon/pixelformat.h>

#include <memory>

#include <ddktl/device.h>

#include "virtio_gpu.h"

namespace virtio {

class Ring;

class GpuDevice;
using DeviceType = ddk::Device<GpuDevice, ddk::GetProtocolable>;
class GpuDevice : public Device,
                  public DeviceType,
                  public ddk::DisplayControllerImplProtocol<GpuDevice, ddk::base_protocol> {
 public:
  GpuDevice(zx_device_t* device, zx::bti bti, std::unique_ptr<Backend> backend);
  ~GpuDevice() override;

  zx_status_t Init() override;
  zx_status_t DdkGetProtocol(uint32_t proto_id, void* out);
  void DdkRelease() { virtio::Device::Release(); }

  void IrqRingUpdate() override;
  void IrqConfigChange() override;

  const virtio_gpu_resp_display_info::virtio_gpu_display_one* pmode() const { return &pmode_; }

  void Flush();

  const char* tag() const override { return "virtio-gpu"; }

  struct BufferInfo {
    zx::vmo vmo = {};
    size_t offset = 0;
    uint32_t bytes_per_pixel = 0;
    uint32_t bytes_per_row = 0;
  };
  zx::result<BufferInfo> GetAllocatedBufferInfoForImage(uint64_t collection_id, uint32_t index,
                                                        const image_t* image) const;

  void DisplayControllerImplSetDisplayControllerInterface(
      const display_controller_interface_protocol_t* intf);

  zx_status_t DisplayControllerImplSetDisplayCaptureInterface(
      const display_capture_interface_protocol_t* intf) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  zx_status_t DisplayControllerImplImportBufferCollection(uint64_t collection_id,
                                                          zx::channel collection_token);
  zx_status_t DisplayControllerImplReleaseBufferCollection(uint64_t collection_id);

  zx_status_t DisplayControllerImplImportImage(image_t* image, zx_unowned_handle_t handle,
                                               uint32_t index) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  zx_status_t DisplayControllerImplImportImage2(image_t* image, uint64_t collection_id,
                                                uint32_t index);

  zx_status_t DisplayControllerImplImportImageForCapture(zx_unowned_handle_t collection_handle,
                                                         uint32_t index,
                                                         uint64_t* out_capture_handle) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  zx_status_t DisplayControllerImplImportImageForCapture2(uint64_t collection_id, uint32_t index,
                                                          uint64_t* out_capture_handle) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  void DisplayControllerImplReleaseImage(image_t* image);

  uint32_t DisplayControllerImplCheckConfiguration(const display_config_t** display_configs,
                                                   size_t display_count,
                                                   uint32_t** layer_cfg_results,
                                                   size_t* layer_cfg_result_count);

  void DisplayControllerImplApplyConfiguration(const display_config_t** display_configs,
                                               size_t display_count,
                                               const config_stamp_t* config_stamp);

  void DisplayControllerImplSetEld(uint64_t display_id, const uint8_t* raw_eld_list,
                                   size_t raw_eld_count) {}  // No ELD required for non-HDA systems.
  zx_status_t DisplayControllerImplGetSysmemConnection(zx::channel sysmem_handle);

  zx_status_t DisplayControllerImplSetBufferCollectionConstraints(const image_t* config,
                                                                  zx_unowned_handle_t collection) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  zx_status_t DisplayControllerImplSetBufferCollectionConstraints2(const image_t* config,
                                                                   uint64_t collection_id);
  zx_status_t DisplayControllerImplSetDisplayPower(uint64_t display_id, bool power_on);

  zx_status_t DisplayControllerImplStartCapture(uint64_t capture_handle) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  zx_status_t DisplayControllerImplReleaseCapture(uint64_t capture_handle) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  bool DisplayControllerImplIsCaptureCompleted() { return false; }

  zx_status_t SetAndInitSysmemForTesting(
      fidl::WireSyncClient<fuchsia_hardware_sysmem::Sysmem> sysmem) {
    sysmem_ = std::move(sysmem);
    return InitSysmemAllocatorClient();
  }

 private:
  // Internal routines
  template <typename RequestType, typename ResponseType>
  void send_command_response(const RequestType* cmd, ResponseType** res);
  zx_status_t Import(zx::vmo vmo, image_t* image, size_t offset, uint32_t pixel_size,
                     uint32_t row_bytes);

  zx_status_t get_display_info();
  zx_status_t allocate_2d_resource(uint32_t* resource_id, uint32_t width, uint32_t height,
                                   zx_pixel_format_t pixel_format);
  zx_status_t attach_backing(uint32_t resource_id, zx_paddr_t ptr, size_t buf_len);
  zx_status_t set_scanout(uint32_t scanout_id, uint32_t resource_id, uint32_t width,
                          uint32_t height);
  zx_status_t flush_resource(uint32_t resource_id, uint32_t width, uint32_t height);
  zx_status_t transfer_to_host_2d(uint32_t resource_id, uint32_t width, uint32_t height);

  zx_status_t virtio_gpu_start();

  // Initializes the sysmem Allocator client used to import incoming buffer
  // collection tokens.
  //
  // On success, returns ZX_OK and the sysmem allocator client will be open
  // until the device is released.
  zx_status_t InitSysmemAllocatorClient();

  thrd_t start_thread_ = {};

  // the main virtio ring
  Ring vring_ = {this};

  // gpu op
  io_buffer_t gpu_req_ = {};

  // A saved copy of the display
  virtio_gpu_resp_display_info::virtio_gpu_display_one pmode_ = {};
  int pmode_id_ = -1;

  uint32_t next_resource_id_ = 1;

  fbl::Mutex request_lock_;
  sem_t request_sem_ = {};
  sem_t response_sem_ = {};

  // Flush thread
  void virtio_gpu_flusher();
  thrd_t flush_thread_ = {};
  fbl::Mutex flush_lock_;
  cnd_t flush_cond_ = {};
  bool flush_pending_ = false;

  display_controller_interface_protocol_t dc_intf_ = {};
  fidl::WireSyncClient<fuchsia_hardware_sysmem::Sysmem> sysmem_;

  // The sysmem allocator client used to bind incoming buffer collection tokens.
  fidl::WireSyncClient<fuchsia_sysmem::Allocator> sysmem_allocator_client_;

  // Imported sysmem buffer collections.
  std::unordered_map<uint64_t, fidl::WireSyncClient<fuchsia_sysmem::BufferCollection>>
      buffer_collections_;

  struct imported_image* latest_fb_ = nullptr;
  struct imported_image* displayed_fb_ = nullptr;
  config_stamp_t latest_config_stamp_ = {.value = INVALID_CONFIG_STAMP_VALUE};
  config_stamp_t displayed_config_stamp_ = {.value = INVALID_CONFIG_STAMP_VALUE};

  // TODO(fxbug.dev/122802): Support more formats.
  static constexpr std::array<zx_pixel_format_t, 2> kSupportedFormats = {
      ZX_PIXEL_FORMAT_RGB_x888,
      ZX_PIXEL_FORMAT_ARGB_8888,
  };
};

}  // namespace virtio

#endif  // SRC_GRAPHICS_DRIVERS_VIRTIO_GPU_H_
