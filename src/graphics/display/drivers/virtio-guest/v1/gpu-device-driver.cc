// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/virtio-guest/v1/gpu-device-driver.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/driver.h>
#include <lib/virtio/driver_utils.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include <cstring>
#include <memory>
#include <utility>

#include <ddktl/device.h>
#include <ddktl/init-txn.h>
#include <fbl/alloc_checker.h>

#include "src/graphics/display/drivers/virtio-guest/v1/display-controller-banjo.h"
#include "src/graphics/display/drivers/virtio-guest/v1/display-coordinator-events-banjo.h"
#include "src/graphics/display/drivers/virtio-guest/v1/display-engine.h"

namespace virtio_display {

// static
zx_status_t GpuDeviceDriver::Create(zx_device_t* parent) {
  fbl::AllocChecker alloc_checker;
  auto coordinator_events = fbl::make_unique_checked<DisplayCoordinatorEventsBanjo>(&alloc_checker);
  if (!alloc_checker.check()) {
    zxlogf(ERROR, "Failed to allocate memory for DisplayCoordinatorEventsBanjo");
    return ZX_ERR_NO_MEMORY;
  }

  zx::result<std::unique_ptr<DisplayEngine>> display_engine_result =
      DisplayEngine::Create(parent, coordinator_events.get());
  if (display_engine_result.is_error()) {
    // DisplayEngine::Create() logs on error.
    return display_engine_result.error_value();
  }

  auto driver = fbl::make_unique_checked<GpuDeviceDriver>(&alloc_checker, parent,
                                                          std::move(coordinator_events),
                                                          std::move(display_engine_result).value());
  if (!alloc_checker.check()) {
    zxlogf(ERROR, "Failed to allocate memory for GpuDeviceDriver");
    return ZX_ERR_NO_MEMORY;
  }

  zx::result<> init_result = driver->Init();
  if (init_result.is_error()) {
    // DisplayEngine::Init() logs on error.
    return init_result.error_value();
  }

  // devmgr now owns the memory for `device`.
  [[maybe_unused]] auto device_ptr = driver.release();
  return ZX_OK;
}

GpuDeviceDriver::GpuDeviceDriver(zx_device_t* bus_device,
                                 std::unique_ptr<DisplayCoordinatorEventsBanjo> coordinator_events,
                                 std::unique_ptr<DisplayEngine> display_engine)
    : DdkDeviceType(bus_device),
      coordinator_events_(std::move(coordinator_events)),
      display_engine_(std::move(display_engine)),
      display_controller_banjo_(display_engine_.get(), coordinator_events_.get()) {
  ZX_DEBUG_ASSERT(coordinator_events_);
  ZX_DEBUG_ASSERT(display_engine_);
}

GpuDeviceDriver::~GpuDeviceDriver() {
  if (start_thread_.joinable()) {
    start_thread_.join();
  }
}

zx::result<> GpuDeviceDriver::Init() {
  zx_status_t status = DdkAdd(
      ddk::DeviceAddArgs("virtio-gpu-display").set_proto_id(ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL));
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to add device node: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  gpu_control_server_ = std::make_unique<GpuControlServer>(
      this, display_engine_->pci_device().GetCapabilitySetLimit());

  zx::result<> result = gpu_control_server_->Init(parent());
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to init virtio gpu server: %s", result.status_string());
    return zx::error(result.status_value());
  }

  zxlogf(TRACE, "GpuDeviceDriver::Init success");

  return zx::ok();
}

zx_status_t GpuDeviceDriver::DdkGetProtocol(uint32_t proto_id, void* out) {
  return display_controller_banjo_.DdkGetProtocol(proto_id, out);
}

void GpuDeviceDriver::DdkInit(ddk::InitTxn txn) {
  start_thread_ = std::thread([this, init_txn = std::move(txn)]() mutable {
    zx_status_t status = display_engine_->Start();
    init_txn.Reply(status);
  });
}

void GpuDeviceDriver::DdkRelease() { delete this; }

void GpuDeviceDriver::SendHardwareCommand(cpp20::span<uint8_t> request,
                                          std::function<void(cpp20::span<uint8_t>)> callback) {
  display_engine_->pci_device().ExchangeControlqVariableLengthRequestResponse(std::move(request),
                                                                              std::move(callback));
}

namespace {

constexpr zx_driver_ops_t kDriverOps = {
    .version = DRIVER_OPS_VERSION,
    .bind = [](void* ctx, zx_device_t* parent) { return GpuDeviceDriver::Create(parent); },
};

}  // namespace

}  // namespace virtio_display

ZIRCON_DRIVER(virtio_gpu, virtio_display::kDriverOps, "zircon", "0.1");
