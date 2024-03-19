// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V2_GPU_DEVICE_DRIVER_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V2_GPU_DEVICE_DRIVER_H_

#include <fidl/fuchsia.hardware.sysmem/cpp/wire.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/zx/vmo.h>
#include <semaphore.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>

#include <cstdlib>
#include <memory>

#include <fbl/mutex.h>

#include "src/graphics/lib/virtio/virtio-abi.h"

namespace virtio_display {

class GpuDevice;

// Driver instance that binds to the VIRTIO GPU device.
//
// This class is responsible for interfacing with the Fuchsia Driver Framework.
class GpuDeviceDriver : public fdf::DriverBase {
 public:
  GpuDeviceDriver(fdf::DriverStartArgs start_args,
                  fdf::UnownedSynchronizedDispatcher driver_dispatcher);
  ~GpuDeviceDriver() override;

  // Asynchronous start.
  void Start(fdf::StartCompleter completer) override;
  void Stop() override;
  void PrepareStop(fdf::PrepareStopCompleter completer) override;

  const virtio_abi::ScanoutInfo* pmode() const { return &pmode_; }

 private:
  // Internal routines.
  zx_status_t get_display_info();
  zx_status_t allocate_2d_resource(uint32_t* resource_id, uint32_t width, uint32_t height,
                                   fuchsia_images2::wire::PixelFormat pixel_format);
  zx_status_t attach_backing(uint32_t resource_id, zx_paddr_t ptr, size_t buf_len);
  zx_status_t set_scanout(uint32_t scanout_id, uint32_t resource_id, uint32_t width,
                          uint32_t height);
  zx_status_t flush_resource(uint32_t resource_id, uint32_t width, uint32_t height);
  zx_status_t transfer_to_host_2d(uint32_t resource_id, uint32_t width, uint32_t height);

  zx_status_t Stage2Init();

  static std::string name() { return "virtio-gpu-display"; }
  std::unique_ptr<GpuDevice> device_;
  fidl::WireSyncClient<fuchsia_driver_framework::Node> parent_node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;
  compat::SyncInitializedDeviceServer compat_server_;

  // A saved copy of the display.
  virtio_abi::ScanoutInfo pmode_ = {};
  int pmode_id_ = -1;

  uint32_t next_resource_id_ = 1;

  fbl::Mutex request_lock_;

  // The sysmem allocator client used to bind incoming buffer collection tokens.
  fidl::WireSyncClient<fuchsia_sysmem::Allocator> sysmem_;
};

}  // namespace virtio_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V2_GPU_DEVICE_DRIVER_H_
