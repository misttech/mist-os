// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/coordinator-driver.h"

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/zx/result.h>
#include <zircon/types.h>

#include <ddktl/device.h>
#include <fbl/alloc_checker.h>

#include "src/graphics/display/drivers/coordinator/controller.h"

namespace display {

// static
zx::result<> CoordinatorDriver::Create(zx_device_t* parent) {
  auto create_engine_driver_client_result = EngineDriverClient::Create(parent);
  if (create_engine_driver_client_result.is_error()) {
    zxlogf(ERROR, "Failed to create EngineDriverClient: %s",
           create_engine_driver_client_result.status_string());
    return create_engine_driver_client_result.take_error();
  }

  zx::result<std::unique_ptr<Controller>> create_controller_result =
      Controller::Create(std::move(create_engine_driver_client_result).value());
  if (create_controller_result.is_error()) {
    zxlogf(ERROR, "Failed to create Controller: %s", create_controller_result.status_string());
    return create_controller_result.take_error();
  }

  fbl::AllocChecker alloc_checker;
  auto coordinator_driver = fbl::make_unique_checked<CoordinatorDriver>(
      &alloc_checker, parent, std::move(create_controller_result).value());
  if (!alloc_checker.check()) {
    zxlogf(ERROR, "Failed to allocate memory for CoordinatorDriver");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  zx::result<> bind_result = coordinator_driver->Bind();
  if (bind_result.is_error()) {
    zxlogf(ERROR, "Failed to bind the Coordinator driver: %s", bind_result.status_string());
    return bind_result.take_error();
  }

  // `coordinator_driver` is now managed by the driver manager.
  [[maybe_unused]] CoordinatorDriver* released_coordinator_driver = coordinator_driver.release();

  return zx::ok();
}

CoordinatorDriver::CoordinatorDriver(zx_device_t* parent, std::unique_ptr<Controller> controller)
    : DeviceType(parent), controller_(std::move(controller)) {
  ZX_DEBUG_ASSERT(controller_ != nullptr);
}

CoordinatorDriver::~CoordinatorDriver() = default;

zx::result<> CoordinatorDriver::Bind() {
  zx_status_t status = DdkAdd(ddk::DeviceAddArgs("display-coordinator")
                                  .set_flags(DEVICE_ADD_NON_BINDABLE)
                                  .set_inspect_vmo(controller_->inspector().DuplicateVmo()));
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to add display coordinator device: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  return zx::ok();
}

void CoordinatorDriver::DdkUnbind(ddk::UnbindTxn txn) {
  controller_->PrepareStop();
  txn.Reply();
}

void CoordinatorDriver::DdkRelease() {
  controller_->Stop();
  delete this;
}

void CoordinatorDriver::OpenCoordinatorForVirtcon(
    OpenCoordinatorForVirtconRequestView request,
    OpenCoordinatorForVirtconCompleter::Sync& completer) {
  controller_->OpenCoordinatorForVirtcon(std::move(request), completer);
}

void CoordinatorDriver::OpenCoordinatorForPrimary(
    OpenCoordinatorForPrimaryRequestView request,
    OpenCoordinatorForPrimaryCompleter::Sync& completer) {
  controller_->OpenCoordinatorForPrimary(std::move(request), completer);
}

static constexpr zx_driver_ops_t kCoordinatorDriverOps = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = [](void* ctx, zx_device_t* device) {
    return CoordinatorDriver::Create(device).status_value();
  };
  return ops;
}();

}  // namespace display

ZIRCON_DRIVER(display_controller, display::kCoordinatorDriverOps, "zircon", "0.1");
