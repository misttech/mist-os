// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/virtio-guest/v1/gpu-control-server.h"

#include <lib/ddk/debug.h>
#include <lib/fdf/cpp/dispatcher.h>

namespace virtio_display {

GpuControlServer::GpuControlServer(Owner* owner, size_t capability_set_limit)
    : owner_(owner),
      outgoing_(fdf::Dispatcher::GetCurrent()->async_dispatcher()),
      capability_set_limit_(capability_set_limit) {}

zx::result<> GpuControlServer::Init(zx_device_t* parent_device) {
  auto* dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher();

  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    zxlogf(ERROR, "CreateEndpoints failed: %s", endpoints.status_string());
    return zx::error(endpoints.status_value());
  }

  zx::result result = outgoing_.AddService<fuchsia_gpu_virtio::Service>(
      fuchsia_gpu_virtio::Service::InstanceHandler({
          .control = bindings_.CreateHandler(this, dispatcher, fidl::kIgnoreBindingClosure),
      }));
  if (result.is_error()) {
    zxlogf(ERROR, "AddService failed: %s", result.status_string());
    return zx::error(result.status_value());
  }

  result = outgoing_.Serve(std::move(endpoints->server));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to serve the outgoing directory: %s", result.status_string());
    return zx::error(result.status_value());
  }

  std::array offers = {fuchsia_gpu_virtio::Service::Name};

  device_add_args_t args = {.name = "virtio-gpu-control",
                            .ctx = this,
                            .fidl_service_offers = offers.data(),
                            .fidl_service_offer_count = offers.size(),
                            .flags = 0,
                            .outgoing_dir_channel = endpoints->client.TakeChannel().release()};

  zx_status_t status = device_add(parent_device, &args, &gpu_control_device_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to add gpu server node: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  return zx::ok();
}

void GpuControlServer::GetCapabilitySetLimit(GetCapabilitySetLimitCompleter::Sync& completer) {
  zxlogf(TRACE, "GpuControlServer::GetCapabilitySetLimit returning %lu", capability_set_limit_);

  completer.Reply(capability_set_limit_);
}

void GpuControlServer::SendHardwareCommand(
    fuchsia_gpu_virtio::wire::GpuControlSendHardwareCommandRequest* request,
    SendHardwareCommandCompleter::Sync& completer) {
  zxlogf(TRACE, "GpuControlServer::SendHardwareCommand");

  auto callback = [&completer](cpp20::span<uint8_t> response) {
    completer.ReplySuccess(
        fidl::VectorView<uint8_t>::FromExternal(response.data(), response.size()));
  };

  owner_->SendHardwareCommand(
      cpp20::span<uint8_t>(request->request.data(), request->request.count()), std::move(callback));
}

}  // namespace virtio_display
