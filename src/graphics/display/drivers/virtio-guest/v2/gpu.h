// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V2_GPU_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V2_GPU_H_

#include <fidl/fuchsia.hardware.sysmem/cpp/wire.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/virtio/device.h>
#include <lib/virtio/ring.h>
#include <lib/zx/vmo.h>
#include <semaphore.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>

#include <cstdlib>
#include <memory>

#include "src/graphics/display/drivers/virtio-guest/v2/virtio-abi.h"

namespace virtio {

class Ring;

class GpuDriver : public fdf::DriverBase {
 public:
  GpuDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher);
  ~GpuDriver() override;

  class Device : public virtio::Device {
   public:
    Device(zx::bti bti, std::unique_ptr<Backend> backend);
    ~Device();

    static fit::result<zx_status_t, std::unique_ptr<Device>> Create(
        fidl::ClientEnd<fuchsia_hardware_pci::Device> client_end);

    zx_status_t Init() override;
    void IrqRingUpdate() override;
    void IrqConfigChange() override;
    const char* tag() const override { return "virtio-gpu"; }

    static uint64_t GetRequestSize(zx::vmo& vmo);

    template <typename RequestType, typename ResponseType>
    void send_command_response(const RequestType* cmd, ResponseType** res);

   private:
    sem_t request_sem_ = {};
    sem_t response_sem_ = {};
    Ring vring_ = {this};
    zx::vmo request_vmo_;
    zx::pmt request_pmt_;
    zx_paddr_t request_phys_addr_ = {};
    zx_vaddr_t request_virt_addr_ = {};
    std::optional<uint32_t> capset_count_;
  };

  // Asynchronous start.
  void Start(fdf::StartCompleter completer) override;
  void Stop() override;

  const virtio_abi::ScanoutInfo* pmode() const { return &pmode_; }

  zx_status_t SetAndInitSysmemForTesting(
      fidl::WireSyncClient<fuchsia_hardware_sysmem::Sysmem> sysmem) {
    sysmem_ = std::move(sysmem);
    return InitSysmemAllocatorClient();
  }

 private:
  // Internal routines
  zx_status_t get_display_info();
  zx_status_t allocate_2d_resource(uint32_t* resource_id, uint32_t width, uint32_t height,
                                   fuchsia_images2::wire::PixelFormat pixel_format);
  zx_status_t attach_backing(uint32_t resource_id, zx_paddr_t ptr, size_t buf_len);
  zx_status_t set_scanout(uint32_t scanout_id, uint32_t resource_id, uint32_t width,
                          uint32_t height);
  zx_status_t flush_resource(uint32_t resource_id, uint32_t width, uint32_t height);
  zx_status_t transfer_to_host_2d(uint32_t resource_id, uint32_t width, uint32_t height);

  zx_status_t Stage2Init();

  // Initializes the sysmem Allocator client used to import incoming buffer
  // collection tokens.
  //
  // On success, returns ZX_OK and the sysmem allocator client will be open
  // until the device is released.
  zx_status_t InitSysmemAllocatorClient();

  std::unique_ptr<Device> device_;
  fidl::WireSyncClient<fuchsia_driver_framework::Node> parent_node_;

  // A saved copy of the display
  virtio_abi::ScanoutInfo pmode_ = {};
  int pmode_id_ = -1;

  uint32_t next_resource_id_ = 1;

  fbl::Mutex request_lock_;

  fidl::WireSyncClient<fuchsia_hardware_sysmem::Sysmem> sysmem_;

  // The sysmem allocator client used to bind incoming buffer collection tokens.
  fidl::WireSyncClient<fuchsia_sysmem::Allocator> sysmem_allocator_client_;
};

}  // namespace virtio

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V2_GPU_H_
