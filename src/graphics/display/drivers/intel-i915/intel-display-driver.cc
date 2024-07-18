// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/intel-i915/intel-display-driver.h"

#include <fidl/fuchsia.hardware.pci/cpp/fidl.h>
#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/driver/component/cpp/prepare_stop_completer.h>
#include <lib/driver/component/cpp/start_completer.h>
#include <lib/zx/result.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include <memory>

#include <ddktl/device.h>
#include <fbl/alloc_checker.h>
#include <src/graphics/display/drivers/intel-i915/intel-i915.h>

namespace i915 {

namespace {

constexpr zx_protocol_device_t kGpuCoreDeviceProtocol = {
    .version = DEVICE_OPS_VERSION,
    .get_protocol = [](void* ctx, uint32_t proto_id, void* protocol) -> zx_status_t {
      if (proto_id != ZX_PROTOCOL_INTEL_GPU_CORE) {
        return ZX_ERR_NOT_SUPPORTED;
      }
      IntelDisplayDriver& intel_display_driver = *reinterpret_cast<IntelDisplayDriver*>(ctx);
      zx::result<ddk::AnyProtocol> result = intel_display_driver.GetProtocol(proto_id);
      if (result.is_error()) {
        return result.error_value();
      }
      *reinterpret_cast<ddk::AnyProtocol*>(protocol) = result.value();
      return ZX_OK;
    },
    .release = [](void* ctx) {},
};

constexpr zx_protocol_device_t kDisplayControllerDeviceProtocol = {
    .version = DEVICE_OPS_VERSION,
    .get_protocol = [](void* ctx, uint32_t proto_id, void* protocol) -> zx_status_t {
      if (proto_id != ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL) {
        return ZX_ERR_NOT_SUPPORTED;
      }
      IntelDisplayDriver& intel_display_driver = *reinterpret_cast<IntelDisplayDriver*>(ctx);
      zx::result<ddk::AnyProtocol> result = intel_display_driver.GetProtocol(proto_id);
      if (result.is_error()) {
        return result.error_value();
      }
      *reinterpret_cast<ddk::AnyProtocol*>(protocol) = result.value();
      return ZX_OK;
    },
    .release = [](void* ctx) {},
};

ControllerResources GetControllerResources(zx_device_t* parent) {
  return {
      .framebuffer_resource = zx::unowned_resource(get_framebuffer_resource(parent)),
      .mmio_resource = zx::unowned_resource(get_mmio_resource(parent)),
      .ioport_resource = zx::unowned_resource(get_ioport_resource(parent)),
  };
}

}  // namespace

IntelDisplayDriver::IntelDisplayDriver(zx_device_t* parent, std::unique_ptr<Controller> controller)
    : DeviceType(parent), controller_(std::move(controller)) {}

IntelDisplayDriver::~IntelDisplayDriver() = default;

// static
zx::result<> IntelDisplayDriver::Create(zx_device_t* parent) {
  zx::result<fidl::ClientEnd<fuchsia_sysmem2::Allocator>> sysmem_result =
      ddk::Device<void>::DdkConnectNsProtocol<fuchsia_sysmem2::Allocator>(parent);
  if (sysmem_result.is_error()) {
    zxlogf(ERROR, "Failed to connect to sysmem protocol: %s", sysmem_result.status_string());
    return sysmem_result.take_error();
  }
  fidl::ClientEnd<fuchsia_sysmem2::Allocator> sysmem = std::move(sysmem_result).value();

  zx::result<fidl::ClientEnd<fuchsia_hardware_pci::Device>> pci_result =
      ddk::Device<void>::DdkConnectFragmentFidlProtocol<fuchsia_hardware_pci::Service::Device>(
          parent, "pci");
  if (pci_result.is_error()) {
    zxlogf(ERROR, "Failed to connect to pci protocol: %s", pci_result.status_string());
    return pci_result.take_error();
  }
  fidl::ClientEnd<fuchsia_hardware_pci::Device> pci = std::move(pci_result).value();

  ControllerResources resources = GetControllerResources(parent);

  zx::result<std::unique_ptr<Controller>> controller_result = Controller::Create(
      std::move(sysmem), std::move(pci), std::move(resources), inspect::Inspector{});
  if (controller_result.is_error()) {
    return controller_result.take_error();
  }

  fbl::AllocChecker alloc_checker;
  auto intel_display_driver = fbl::make_unique_checked<IntelDisplayDriver>(
      &alloc_checker, parent, std::move(controller_result).value());
  if (!alloc_checker.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  zx::result<> bind_result = intel_display_driver->Bind();
  if (bind_result.is_error()) {
    return bind_result.take_error();
  }

  // `intel_display_driver` is now managed by the driver manager.
  [[maybe_unused]] IntelDisplayDriver* released_intel_display_driver =
      intel_display_driver.release();
  return zx::ok();
}

void IntelDisplayDriver::DdkInit(ddk::InitTxn txn) {
  fdf::StartCompleter completer(
      [txn = std::move(txn)](zx::result<> result) mutable { txn.Reply(result.status_value()); });
  controller_->Start(std::move(completer));
}

void IntelDisplayDriver::DdkUnbind(ddk::UnbindTxn txn) {
  device_async_remove(gpu_core_device_);
  device_async_remove(display_controller_device_);

  fdf::PrepareStopCompleter completer([txn = std::move(txn)](zx::result<> result) mutable {
    if (result.is_error()) {
      zxlogf(WARNING, "Failed to unbind the Controller: %s", result.status_string());
    }
    txn.Reply();
  });
  controller_->PrepareStopOnPowerOn(std::move(completer));
}

void IntelDisplayDriver::DdkRelease() { delete this; }

void IntelDisplayDriver::DdkSuspend(ddk::SuspendTxn txn) {
  fdf::PrepareStopCompleter completer([txn = std::move(txn)](zx::result<> result) mutable {
    txn.Reply(result.status_value(), txn.requested_state());
  });
  controller_->PrepareStopOnSuspend(txn.suspend_reason(), std::move(completer));
}

zx::result<ddk::AnyProtocol> IntelDisplayDriver::GetProtocol(uint32_t proto_id) {
  return controller_->GetProtocol(proto_id);
}

zx::result<> IntelDisplayDriver::Bind() {
  zx_status_t add_root_device_status =
      DdkAdd(ddk::DeviceAddArgs("intel_i915")
                 .set_inspect_vmo(controller_->inspector().DuplicateVmo())
                 .set_flags(DEVICE_ADD_NON_BINDABLE));
  if (add_root_device_status != ZX_OK) {
    zxlogf(ERROR, "Failed to add root i915 device: %s",
           zx_status_get_string(add_root_device_status));
    return zx::error(add_root_device_status);
  }

  device_add_args_t display_device_add_args = {
      .version = DEVICE_ADD_ARGS_VERSION,
      .name = "intel-display-controller",
      .ctx = this,
      .ops = &kDisplayControllerDeviceProtocol,
      .proto_id = ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL,
  };
  zx_status_t add_display_device_status =
      device_add(zxdev(), &display_device_add_args, &display_controller_device_);
  if (add_display_device_status != ZX_OK) {
    zxlogf(ERROR, "Failed to publish display controller device: %s",
           zx_status_get_string(add_display_device_status));
    return zx::error(add_display_device_status);
  }

  device_add_args_t gpu_device_add_args = {
      .version = DEVICE_ADD_ARGS_VERSION,
      .name = "intel-gpu-core",
      .ctx = this,
      .ops = &kGpuCoreDeviceProtocol,
      .proto_id = ZX_PROTOCOL_INTEL_GPU_CORE,
  };
  zx_status_t add_gpu_device_status = device_add(zxdev(), &gpu_device_add_args, &gpu_core_device_);
  if (add_gpu_device_status != ZX_OK) {
    zxlogf(ERROR, "Failed to publish gpu core device: %s",
           zx_status_get_string(add_gpu_device_status));
    return zx::error(add_gpu_device_status);
  }

  return zx::ok();
}

namespace {

constexpr zx_driver_ops_t kDriverOps = {
    .version = DRIVER_OPS_VERSION,
    .bind = [](void* ctx,
               zx_device_t* parent) { return IntelDisplayDriver::Create(parent).status_value(); },
};

}  // namespace

}  // namespace i915

ZIRCON_DRIVER(intel_i915, i915::kDriverOps, "zircon", "0.1");
