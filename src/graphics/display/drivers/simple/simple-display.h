// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_SIMPLE_SIMPLE_DISPLAY_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_SIMPLE_SIMPLE_DISPLAY_H_

#include <lib/ddk/driver.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>

#include <fbl/mutex.h>

#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types-cpp/driver-buffer-collection-id.h"

#if __cplusplus

#include <fidl/fuchsia.hardware.sysmem/cpp/wire.h>
#include <fidl/fuchsia.images2/cpp/wire.h>
#include <fidl/fuchsia.sysmem/cpp/wire.h>
#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async/cpp/task.h>
#include <lib/fidl/cpp/wire/server.h>
#include <lib/fit/function.h>
#include <lib/mmio/mmio.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/clock.h>
#include <lib/zx/vmo.h>

#include <atomic>
#include <memory>

#include <ddktl/device.h>

class SimpleDisplay;
using DeviceType = ddk::Device<SimpleDisplay>;
using HeapServer = fidl::WireServer<fuchsia_sysmem2::Heap>;

class SimpleDisplay : public DeviceType,
                      public HeapServer,
                      public ddk::DisplayControllerImplProtocol<SimpleDisplay, ddk::base_protocol> {
 public:
  SimpleDisplay(zx_device_t* parent, fidl::WireSyncClient<fuchsia_hardware_sysmem::Sysmem> sysmem,
                fdf::MmioBuffer framebuffer_mmio, uint32_t width, uint32_t height, uint32_t stride,
                fuchsia_images2::wire::PixelFormat format);
  ~SimpleDisplay() = default;

  void DdkRelease();
  zx_status_t Bind(const char* name, std::unique_ptr<SimpleDisplay>* controller_ptr);

  void AllocateVmo(AllocateVmoRequestView request, AllocateVmoCompleter::Sync& completer) override;
  void CreateResource(CreateResourceRequestView request,
                      CreateResourceCompleter::Sync& completer) override;
  void DestroyResource(DestroyResourceRequestView request,
                       DestroyResourceCompleter::Sync& completer) override;

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
  zx_status_t DisplayControllerImplImportImage(image_t* image,
                                               uint64_t banjo_driver_buffer_collection_id,
                                               uint32_t index);
  zx_status_t DisplayControllerImplImportImageForCapture(uint64_t banjo_driver_buffer_collection_id,
                                                         uint32_t index,
                                                         uint64_t* out_capture_handle) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  void DisplayControllerImplReleaseImage(image_t* image);
  config_check_result_t DisplayControllerImplCheckConfiguration(
      const display_config_t** display_configs, size_t display_count, uint32_t** layer_cfg_results,
      size_t* layer_cfg_result_count);
  void DisplayControllerImplApplyConfiguration(const display_config_t** display_config,
                                               size_t display_count,
                                               const config_stamp_t* banjo_config_stamp);
  void DisplayControllerImplSetEld(uint64_t display_id, const uint8_t* raw_eld_list,
                                   size_t raw_eld_count) {}  // No ELD required for non-HDA systems.
  zx_status_t DisplayControllerImplGetSysmemConnection(zx::channel connection);
  zx_status_t DisplayControllerImplSetBufferCollectionConstraints(
      const image_t* config, uint64_t banjo_driver_buffer_collection_id);
  zx_status_t DisplayControllerImplSetDisplayPower(uint64_t display_id, bool power_on) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  zx_status_t DisplayControllerImplStartCapture(uint64_t capture_handle) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  zx_status_t DisplayControllerImplReleaseCapture(uint64_t capture_handle) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  bool DisplayControllerImplIsCaptureCompleted() { return false; }

  const std::unordered_map<display::DriverBufferCollectionId,
                           fidl::WireSyncClient<fuchsia_sysmem::BufferCollection>>&
  GetBufferCollectionsForTesting() const {
    return buffer_collections_;
  }

 private:
  zx_status_t InitSysmemAllocatorClient();

  void OnPeriodicVSync();

  fidl::WireSyncClient<fuchsia_hardware_sysmem::Sysmem> sysmem_;

  // The sysmem allocator client used to bind incoming buffer collection tokens.
  fidl::WireSyncClient<fuchsia_sysmem::Allocator> sysmem_allocator_client_;

  // Imported sysmem buffer collections.
  std::unordered_map<display::DriverBufferCollectionId,
                     fidl::WireSyncClient<fuchsia_sysmem::BufferCollection>>
      buffer_collections_;

  async::Loop loop_;

  static_assert(std::atomic<zx_koid_t>::is_always_lock_free);
  std::atomic<zx_koid_t> framebuffer_koid_;
  static_assert(std::atomic<bool>::is_always_lock_free);
  std::atomic<bool> has_image_;

  // A lock is required to ensure the atomicity when setting |config_stamp| in
  // |ApplyConfiguration()| and passing |&config_stamp_| to |OnDisplayVsync()|.
  fbl::Mutex mtx_;
  display::ConfigStamp config_stamp_ TA_GUARDED(mtx_) = display::kInvalidConfigStamp;

  const fdf::MmioBuffer framebuffer_mmio_;
  const uint32_t width_;
  const uint32_t height_;
  const uint32_t stride_;
  const fuchsia_images2::wire::PixelFormat format_;

  const uint64_t kFormatModifier = fuchsia_images2::wire::kFormatModifierLinear;

  // Only used on the vsync thread.
  zx::time next_vsync_time_;
  ddk::DisplayControllerInterfaceProtocolClient intf_;
};

#endif  // __cplusplus

__BEGIN_CDECLS
zx_status_t bind_simple_pci_display(zx_device_t* dev, const char* name, uint32_t bar,
                                    uint32_t width, uint32_t height, uint32_t stride,
                                    fuchsia_images2::wire::PixelFormat format);

zx_status_t bind_simple_fidl_pci_display(zx_device_t* dev, const char* name, uint32_t bar,
                                         uint32_t width, uint32_t height, uint32_t stride,
                                         fuchsia_images2::wire::PixelFormat format);

zx_status_t bind_simple_pci_display_bootloader(zx_device_t* dev, const char* name, uint32_t bar,
                                               bool use_fidl);
__END_CDECLS

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_SIMPLE_SIMPLE_DISPLAY_H_
