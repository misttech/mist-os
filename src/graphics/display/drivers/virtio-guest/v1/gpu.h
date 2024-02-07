// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_GPU_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_GPU_H_

#include <fidl/fuchsia.hardware.sysmem/cpp/wire.h>
#include <fidl/fuchsia.images2/cpp/wire.h>
#include <fidl/fuchsia.sysmem/cpp/wire.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/stdcompat/span.h>
#include <lib/virtio/device.h>
#include <lib/virtio/ring.h>
#include <lib/zx/pmt.h>
#include <lib/zx/result.h>
#include <lib/zx/vmo.h>
#include <semaphore.h>
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
#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types-cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types-cpp/driver-image-id.h"

namespace virtio_display {

class Ring;

class GpuDevice;
using DeviceType = ddk::Device<GpuDevice, ddk::GetProtocolable, ddk::Initializable>;
class GpuDevice : public virtio::Device,
                  public DeviceType,
                  public ddk::DisplayControllerImplProtocol<GpuDevice, ddk::base_protocol> {
 public:
  // Exposed for testing. Production code must use the Create() factory method.
  //
  // `bti` is used to obtain physical memory addresses given to the virtio
  // device.
  //
  // `virtio_queue_buffer_pool` must be large enough to store requests and
  // responses exchanged with the virtio device. The buffer must reside at
  // `virtio_queue_buffer_pool_physical_address` in the virtio device's physical
  // address space.
  //
  // The instance hangs onto `virtio_queue_buffer_pool_vmo` and
  // `virtio_queue_buffer_pool_pin` for the duration of its lifetime. They are
  // intended to keep the memory backing `virtio_queue_buffer_pool` alive and
  // pinned to `virtio_queue_buffer_pool_physical_address`.
  GpuDevice(zx_device_t* bus_device, zx::bti bti, std::unique_ptr<virtio::Backend> backend,
            fidl::ClientEnd<fuchsia_sysmem::Allocator> sysmem_client,
            zx::vmo virtio_queue_buffer_pool_vmo, zx::pmt virtio_queue_buffer_pool_pin,
            zx_paddr_t virtio_queue_buffer_pool_physical_address,
            cpp20::span<uint8_t> virtio_queue_buffer_pool);
  ~GpuDevice() override;

  static zx::result<std::unique_ptr<GpuDevice>> Create(zx_device_t* bus_device);

  zx_status_t Init() override;
  zx_status_t DdkGetProtocol(uint32_t proto_id, void* out);

  void DdkInit(ddk::InitTxn txn);
  void DdkRelease();

  void IrqRingUpdate() override;
  void IrqConfigChange() override;

  const virtio_abi::ScanoutInfo* pmode() const { return &pmode_; }

  const char* tag() const override { return "virtio-gpu"; }

  struct BufferInfo {
    zx::vmo vmo = {};
    size_t offset = 0;
    uint32_t bytes_per_pixel = 0;
    uint32_t bytes_per_row = 0;
    fuchsia_images2::wire::PixelFormat pixel_format;
  };
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
  // Synchronous request/response exchange on the main virtqueue.
  //
  // The returned reference points to data owned by the GpuDevice instance, and
  // is only valid until the method is called again.
  //
  // Call sites are expected to rely on partial template type inference. The
  // argument type should not be specified twice.
  //
  //     const virtio_abi::EmptyRequest request = {...};
  //     const auto& response =
  //         device->ExchangeRequestResponse<virtio_abi::EmptyResponse>(
  //             request);
  //
  // The current implementation fully serializes request/response exchanges.
  // More precisely, even if ExchangeRequestResponse() is called concurrently,
  // the virtio device will never be given a request while another request is
  // pending. While this behavior is observable, it is not part of the API. The
  // implementation may be evolved to issue concurrent requests in the future.
  template <typename ResponseType, typename RequestType>
  const ResponseType& ExchangeRequestResponse(const RequestType& request);

  // Called when the virtio device notifies the driver of having used a buffer.
  //
  // `used_descriptor_index` is the virtqueue descriptor table index pointing to
  // the data that was consumed by the device. The indicated buffer, as well as
  // all the linked buffers, are now owned by the driver.
  void VirtioBufferUsedByDevice(uint32_t used_descriptor_index) __TA_REQUIRES(virtio_queue_mutex_);

  zx::result<display::DriverImageId> Import(zx::vmo vmo, const image_t* image, size_t offset,
                                            uint32_t pixel_size, uint32_t row_bytes,
                                            fuchsia_images2::wire::PixelFormat pixel_format);

  zx_status_t get_display_info();
  zx_status_t allocate_2d_resource(uint32_t* resource_id, uint32_t width, uint32_t height,
                                   fuchsia_images2::wire::PixelFormat pixel_format);
  zx_status_t attach_backing(uint32_t resource_id, zx_paddr_t ptr, size_t buf_len);
  zx_status_t set_scanout(uint32_t scanout_id, uint32_t resource_id, uint32_t width,
                          uint32_t height);
  zx_status_t flush_resource(uint32_t resource_id, uint32_t width, uint32_t height);
  zx_status_t transfer_to_host_2d(uint32_t resource_id, uint32_t width, uint32_t height);

  zx_status_t Start();

  // Initializes the sysmem Allocator client used to import incoming buffer
  // collection tokens.
  //
  // On success, returns ZX_OK and the sysmem allocator client will be open
  // until the device is released.
  zx_status_t InitSysmemAllocatorClient();

  std::thread start_thread_ = {};

  // Ensures that a single ExchangeRequestResponse() call is in progress.
  fbl::Mutex exchange_request_response_mutex_;

  // Protects data members modified by multiple threads.
  fbl::Mutex virtio_queue_mutex_;

  // Signaled when the virtio device consumes a designated virtio queue buffer.
  //
  // The buffer is identified by `virtio_queue_request_index_`.
  fbl::ConditionVariable virtio_queue_buffer_used_signal_;

  // Identifies the request buffer allocated in ExchangeRequestResponse().
  //
  // nullopt iff no ExchangeRequestResponse() is in progress.
  std::optional<uint16_t> virtio_queue_request_index_ __TA_GUARDED(virtio_queue_mutex_);

  // The GPU device's control virtqueue.
  //
  // Defined in the VIRTIO spec Section 5.7.2 "GPU Device" > "Virtqueues".
  virtio::Ring virtio_queue_ __TA_GUARDED(virtio_queue_mutex_);

  // Backs `virtio_queue_buffer_pool_`.
  const zx::vmo virtio_queue_buffer_pool_vmo_;

  // Pins `virtio_queue_buffer_pool_vmo_` at a known physical address.
  const zx::pmt virtio_queue_buffer_pool_pin_;

  // The starting address of `virtio_queue_buffer_pool_`.
  const zx_paddr_t virtio_queue_buffer_pool_physical_address_;

  // Memory pinned at a known physical address, used for virtqueue buffers.
  //
  // The span's data is modified by the driver and by the virtio device.
  const cpp20::span<uint8_t> virtio_queue_buffer_pool_;

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

  // Imported sysmem buffer collections.
  std::unordered_map<display::DriverBufferCollectionId,
                     fidl::WireSyncClient<fuchsia_sysmem::BufferCollection>>
      buffer_collections_;

  struct imported_image* latest_fb_ = nullptr;
  struct imported_image* displayed_fb_ = nullptr;
  display::ConfigStamp latest_config_stamp_ = display::kInvalidConfigStamp;
  display::ConfigStamp displayed_config_stamp_ = display::kInvalidConfigStamp;

  // TODO(https://fxbug.dev/42073721): Support more formats.
  static constexpr std::array<fuchsia_images2_pixel_format_enum_value_t, 1> kSupportedFormats = {
      static_cast<fuchsia_images2_pixel_format_enum_value_t>(
          fuchsia_images2::wire::PixelFormat::kB8G8R8A8),
  };
};

}  // namespace virtio_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_GPU_H_
