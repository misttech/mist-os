// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_GPU_DEVICE_DRIVER_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_GPU_DEVICE_DRIVER_H_

#include <lib/ddk/device.h>
#include <lib/zx/result.h>
#include <zircon/types.h>

#include <cstdint>
#include <memory>
#include <thread>

#include <ddktl/device.h>

#include "src/graphics/display/drivers/virtio-guest/v1/display-engine.h"
#include "src/graphics/display/drivers/virtio-guest/v1/gpu-control-server.h"

namespace virtio_display {

class GpuDeviceDriver;
using DdkDeviceType = ddk::Device<GpuDeviceDriver, ddk::GetProtocolable, ddk::Initializable>;

// Integration between this driver and the Driver Framework.
class GpuDeviceDriver : public DdkDeviceType, public GpuControlServer::Owner {
 public:
  // Factory method used by the device manager glue code.
  static zx_status_t Create(zx_device_t* parent);

  // Exposed for testing. Production code must use the Create() factory method.
  explicit GpuDeviceDriver(zx_device_t* bus_device, std::unique_ptr<DisplayEngine> display_engine);

  GpuDeviceDriver(const GpuDeviceDriver&) = delete;
  GpuDeviceDriver& operator=(const GpuDeviceDriver&) = delete;

  virtual ~GpuDeviceDriver();

  // Resource initialization that is not suitable for the constructor.
  zx::result<> Init();

  // ddk::GetProtocolable interface.
  zx_status_t DdkGetProtocol(uint32_t proto_id, void* out);

  // ddk::Initializable interface.
  void DdkInit(ddk::InitTxn txn);

  // ddk::Device interface.
  void DdkRelease();

  // GpuControlServer::DeviceAccessor interface.
  void SendHardwareCommand(cpp20::span<uint8_t> request,
                           std::function<void(cpp20::span<uint8_t>)> callback) override;

 private:
  std::unique_ptr<DisplayEngine> display_engine_;
  std::unique_ptr<GpuControlServer> gpu_control_server_;

  // Used by DdkInit() for deferred initialization.
  //
  // Not started (and therefore not joinable) until DdkInit() is called.
  std::thread start_thread_;
};

}  // namespace virtio_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_GPU_DEVICE_DRIVER_H_
