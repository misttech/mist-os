// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/virtio-guest/v1/gpu-control-device-driver.h"

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/ddk/debug.h>
#include <lib/fdf/cpp/dispatcher.h>

#include <ddktl/device.h>
#include <fbl/alloc_checker.h>

namespace virtio_display {

GpuControlDeviceDriver::GpuControlDeviceDriver(zx_device_t* parent_device,
                                               GpuControlServer* gpu_control_server,
                                               async_dispatcher_t* dispatcher)
    : ddk::Device<GpuControlDeviceDriver>(parent_device),
      gpu_control_server_(*gpu_control_server),
      dispatcher_(*dispatcher),
      outgoing_(&dispatcher_) {
  ZX_DEBUG_ASSERT(gpu_control_server != nullptr);
  ZX_DEBUG_ASSERT(dispatcher != nullptr);
}

zx::result<fidl::ClientEnd<fuchsia_io::Directory>> GpuControlDeviceDriver::ServeOutgoing() {
  auto [directory_client, directory_server] = fidl::Endpoints<fuchsia_io::Directory>::Create();
  zx::result<> add_service_result = outgoing_.AddService<fuchsia_gpu_virtio::Service>(
      gpu_control_server_.GetInstanceHandler(&dispatcher_));
  if (add_service_result.is_error()) {
    zxlogf(ERROR, "AddService failed: %s", add_service_result.status_string());
    return add_service_result.take_error();
  }

  zx::result<> serve_result = outgoing_.Serve(std::move(directory_server));
  if (serve_result.is_error()) {
    zxlogf(ERROR, "Failed to serve the outgoing directory: %s", serve_result.status_string());
    return serve_result.take_error();
  }

  return zx::ok(std::move(directory_client));
}

zx::result<> GpuControlDeviceDriver::Bind(
    fidl::ClientEnd<fuchsia_io::Directory> outgoing_directory) {
  std::array<const char*, 1> kOffers = {fuchsia_gpu_virtio::Service::Name};
  zx_status_t status = DdkAdd(ddk::DeviceAddArgs("virtio-gpu-control")
                                  .set_fidl_service_offers(kOffers)
                                  .set_flags(0)
                                  .set_outgoing_dir(std::move(outgoing_directory).TakeChannel()));
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to add gpu control node: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  return zx::ok();
}

// static
zx::result<> GpuControlDeviceDriver::Create(zx_device_t* parent_device,
                                            GpuControlServer* gpu_control_server) {
  async_dispatcher_t* dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher();

  fbl::AllocChecker alloc_checker;
  auto gpu_control_driver = fbl::make_unique_checked<GpuControlDeviceDriver>(
      &alloc_checker, parent_device, gpu_control_server, dispatcher);
  if (!alloc_checker.check()) {
    zxlogf(ERROR, "Failed to allocate memory for GpuControlDriver");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> serve_result =
      gpu_control_driver->ServeOutgoing();
  if (serve_result.is_error()) {
    zxlogf(ERROR, "Failed to serve to outgoing directory: %s", serve_result.status_string());
    return serve_result.take_error();
  }
  fidl::ClientEnd<fuchsia_io::Directory> outgoing_directory = std::move(serve_result).value();

  zx::result<> bind_result = gpu_control_driver->Bind(std::move(outgoing_directory));
  if (bind_result.is_error()) {
    zxlogf(ERROR, "Failed to bind to outgoing directory: %s", bind_result.status_string());
    return bind_result.take_error();
  }

  // gpu_control_driver is now managed by the driver manager.
  [[maybe_unused]] GpuControlDeviceDriver* released_driver = gpu_control_driver.release();

  return zx::ok();
}

void GpuControlDeviceDriver::DdkRelease() { delete this; }

}  // namespace virtio_display
