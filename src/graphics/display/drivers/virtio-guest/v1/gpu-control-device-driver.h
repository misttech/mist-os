// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_GPU_CONTROL_DEVICE_DRIVER_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_GPU_CONTROL_DEVICE_DRIVER_H_

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>

#include <ddktl/device.h>

#include "src/graphics/display/drivers/virtio-guest/v1/gpu-control-server.h"

namespace virtio_display {

class GpuControlDeviceDriver : public ddk::Device<GpuControlDeviceDriver> {
 public:
  // Factory method used by the device manager glue code.
  //
  // `parent_device` must be non-null.
  // `gpu_control_server` must be non-null and must outlive the created
  // `GpuControlDeviceDriver`. This can be achieved if the `parent_device` owns
  // the `gpu_control_server`.
  static zx::result<> Create(zx_device_t* parent_device, GpuControlServer* gpu_control_server);

  // Production code must use the `Create` factory method.
  //
  // `gpu_control_server` must be non-null and must outlive the created
  // `GpuControlDeviceDriver`. This can be achieved if the `parent_device` owns
  // the `gpu_control_server`.
  // `dispatcher` must be non-null and outlive the `GpuControlDeviceDriver`.
  explicit GpuControlDeviceDriver(zx_device_t* parent_device, GpuControlServer* gpu_control_server,
                                  async_dispatcher_t* dispatcher);

  ~GpuControlDeviceDriver() = default;

  GpuControlDeviceDriver(const GpuControlDeviceDriver&) = delete;
  GpuControlDeviceDriver& operator=(const GpuControlDeviceDriver&) = delete;
  GpuControlDeviceDriver(GpuControlDeviceDriver&&) = delete;
  GpuControlDeviceDriver& operator=(GpuControlDeviceDriver&&) = delete;

  // ddk::Device:
  void DdkRelease();

 private:
  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> ServeOutgoing();
  zx::result<> Bind(fidl::ClientEnd<fuchsia_io::Directory> outgoing_directory);

  GpuControlServer& gpu_control_server_;

  async_dispatcher_t& dispatcher_;
  component::OutgoingDirectory outgoing_;
};

}  // namespace virtio_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_GPU_CONTROL_DEVICE_DRIVER_H_
