// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GPU_DISPLAY_GPU_DEVICE_DRIVER_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GPU_DISPLAY_GPU_DEVICE_DRIVER_H_

#include <lib/driver/compat/cpp/banjo_server.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/zx/result.h>
#include <zircon/types.h>

#include <cstdint>
#include <memory>
#include <thread>

#include "src/graphics/display/drivers/virtio-gpu-display/display-engine-banjo-adapter.h"
#include "src/graphics/display/drivers/virtio-gpu-display/display-engine-events-banjo.h"
#include "src/graphics/display/drivers/virtio-gpu-display/display-engine.h"
#include "src/graphics/display/drivers/virtio-gpu-display/gpu-control-server.h"

namespace virtio_display {

// Integration between this driver and the Driver Framework.
class GpuDeviceDriver : public fdf::DriverBase, public GpuControlServer::Owner {
 public:
  explicit GpuDeviceDriver(fdf::DriverStartArgs start_args,
                           fdf::UnownedSynchronizedDispatcher driver_dispatcher);

  GpuDeviceDriver(const GpuDeviceDriver&) = delete;
  GpuDeviceDriver& operator=(const GpuDeviceDriver&) = delete;
  GpuDeviceDriver(GpuDeviceDriver&&) = delete;
  GpuDeviceDriver& operator=(GpuDeviceDriver&&) = delete;

  virtual ~GpuDeviceDriver();

  // fdf::DriverBase:
  void Start(fdf::StartCompleter completer) override;
  void Stop() override;

  // GpuControlServer::DeviceAccessor interface.
  void SendHardwareCommand(cpp20::span<uint8_t> request,
                           std::function<void(cpp20::span<uint8_t>)> callback) override;

 private:
  // Resource initialization that is not suitable for the constructor.
  zx::result<> InitResources();

  // Must be called after `InitResources()`.
  zx::result<> InitDisplayNode();

  // Must be called after `InitResources()`.
  zx::result<> InitGpuControlNode();

  // Must outlive `display_engine_` and `engine_banjo_adapter_`.
  std::unique_ptr<DisplayEngineEventsBanjo> engine_events_;

  // Must outlive `engine_banjo_adapter_`.
  std::unique_ptr<DisplayEngine> display_engine_;

  std::unique_ptr<DisplayEngineBanjoAdapter> engine_banjo_adapter_;

  std::unique_ptr<GpuControlServer> gpu_control_server_;

  // Used by Start() for deferred initialization.
  //
  // Not started (and therefore not joinable) until Start() is called.
  std::thread start_thread_;

  compat::SyncInitializedDeviceServer display_compat_server_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> display_node_controller_;

  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> gpu_control_node_controller_;
};

}  // namespace virtio_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GPU_DISPLAY_GPU_DEVICE_DRIVER_H_
