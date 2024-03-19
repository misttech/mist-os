// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/hrtimer/drivers/aml-hrtimer/aml-hrtimer.h"

#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <zircon/syscalls-next.h>

namespace hrtimer {

zx::result<> AmlHrtimer::Start() {
  zx::result pdev_result = incoming()->Connect<fuchsia_hardware_platform_device::Service::Device>();
  if (pdev_result.is_error()) {
    FDF_LOG(ERROR, "Failed to connect to pdev protocol: %s", pdev_result.status_string());
    return pdev_result.take_error();
  }
  fidl::WireSyncClient<fuchsia_hardware_platform_device::Device> pdev(
      std::move(pdev_result.value()));

  auto mmio = pdev->GetMmioById(0);
  if (!mmio.ok()) {
    FDF_LOG(ERROR, "Call to GetMmioById failed: %s", mmio.FormatDescription().c_str());
    return zx::error(mmio.status());
  }
  if (mmio->is_error()) {
    FDF_LOG(ERROR, "GetMmioById failed: %s", zx_status_get_string(mmio->error_value()));
    return mmio->take_error();
  }

  if (!mmio->value()->has_vmo() || !mmio->value()->has_size() || !mmio->value()->has_offset()) {
    FDF_LOG(ERROR, "GetMmioById returned invalid MMIO");
    return zx::error(ZX_ERR_BAD_STATE);
  }

  zx::result mmio_buffer =
      fdf::MmioBuffer::Create(mmio->value()->offset(), mmio->value()->size(),
                              std::move(mmio->value()->vmo()), ZX_CACHE_POLICY_UNCACHED_DEVICE);
  if (mmio_buffer.is_error()) {
    FDF_LOG(ERROR, "Failed to map MMIO: %s", mmio_buffer.status_string());
    return zx::error(mmio_buffer.error_value());
  }

  zx::interrupt irqs[kNumberOfIrqs];
  uint32_t count = 0;
  for (auto& irq : irqs) {
    // In the future we could decide to not set ZX_INTERRUPT_WAKE_VECTOR based on
    // configuration from the board driver.
    auto result = pdev->GetInterruptById(count++, ZX_INTERRUPT_WAKE_VECTOR);
    if (!result.ok()) {
      FDF_LOG(ERROR, "Call to GetInterruptById failed: %s", result.FormatDescription().c_str());
      return zx::error(result->error_value());
    }
    if (result->is_error()) {
      FDF_LOG(ERROR, "GetInterruptById failed: %s", zx_status_get_string(result->error_value()));
      return result->take_error();
    }
    irq = std::move(result->value()->irq);
  }
  server_ = std::make_unique<hrtimer::AmlHrtimerServer>(
      dispatcher(), std::move(*mmio_buffer), std::move(irqs[0]), std::move(irqs[1]),
      std::move(irqs[2]), std::move(irqs[3]), std::move(irqs[4]), std::move(irqs[5]),
      std::move(irqs[6]), std::move(irqs[7]));

  auto result = outgoing()->component().AddUnmanagedProtocol<fuchsia_hardware_hrtimer::Device>(
      bindings_.CreateHandler(server_.get(), dispatcher(), fidl::kIgnoreBindingClosure),
      kDeviceName);
  if (result.is_error()) {
    FDF_LOG(ERROR, "Failed to add input report service: %s", result.status_string());
    return result.take_error();
  }

  if (zx::result result = CreateDevfsNode(); result.is_error()) {
    FDF_LOG(ERROR, "Failed to export to devfs: %s", result.status_string());
    return result.take_error();
  }

  return zx::ok();
}

void AmlHrtimer::PrepareStop(fdf::PrepareStopCompleter completer) {
  server_->ShutDown();
  completer(zx::ok());
}

zx::result<> AmlHrtimer::CreateDevfsNode() {
  fidl::Arena arena;
  zx::result connector = devfs_connector_.Bind(dispatcher());
  if (connector.is_error()) {
    return connector.take_error();
  }

  auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena)
                   .connector(std::move(connector.value()))
                   .class_name("hrtimer");

  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                  .name(arena, kDeviceName)
                  .devfs_args(devfs.Build())
                  .Build();

  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  ZX_ASSERT_MSG(controller_endpoints.is_ok(), "Failed to create node controller endpoints: %s",
                controller_endpoints.status_string());

  zx::result node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
  ZX_ASSERT_MSG(node_endpoints.is_ok(), "Failed to create node endpoints: %s",
                node_endpoints.status_string());

  fidl::WireResult result = fidl::WireCall(node())->AddChild(
      args, std::move(controller_endpoints->server), std::move(node_endpoints->server));
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to add child %s", result.status_string());
    return zx::error(result.status());
  }
  controller_.Bind(std::move(controller_endpoints->client));
  node_.Bind(std::move(node_endpoints->client));
  return zx::ok();
}

}  // namespace hrtimer

FUCHSIA_DRIVER_EXPORT(hrtimer::AmlHrtimer);
