// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/serial/drivers/aml-uart/aml-uart-dfv1.h"

#include <fuchsia/hardware/serial/c/banjo.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/metadata.h>
#include <lib/fit/defer.h>

#include <ddktl/metadata.h>
#include <fbl/alloc_checker.h>

namespace serial {

zx_status_t AmlUartV1::Create(void* ctx, zx_device_t* parent) {
  zx_status_t status;
  auto pdev = ddk::PDevFidl::FromFragment(parent);
  if (!pdev.is_valid()) {
    zxlogf(ERROR, "AmlUart::Create: Could not get pdev");
    return ZX_ERR_NO_RESOURCES;
  }

  zx::result fidl_info = ddk::GetEncodedMetadata<fuchsia_hardware_serial::wire::SerialPortInfo>(
      parent, DEVICE_METADATA_SERIAL_PORT_INFO);
  if (fidl_info.is_error()) {
    zxlogf(ERROR, "device_get_metadata failed: %s", fidl_info.status_string());
    return fidl_info.error_value();
  }

  const serial_port_info_t info{
      .serial_class = static_cast<uint32_t>(fidl_info->serial_class),
      .serial_vid = fidl_info->serial_vid,
      .serial_pid = fidl_info->serial_pid,
  };

  std::optional<fdf::MmioBuffer> mmio;
  status = pdev.MapMmio(0, &mmio);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: pdev_map_&mmio__buffer failed %d", __func__, status);
    return status;
  }

  fbl::AllocChecker ac;
  auto* uart = new (&ac) AmlUartV1(parent);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  return uart->Init(std::move(pdev), info, *std::move(mmio));
}

void AmlUartV1::DdkUnbind(ddk::UnbindTxn txn) {
  if (irq_dispatcher_.has_value()) {
    // The shutdown is async. When it is done, the dispatcher's shutdown callback will complete
    // the unbind txn.
    unbind_txn_.emplace(std::move(txn));
    irq_dispatcher_->ShutdownAsync();
  } else {
    // No inner aml_uart, just reply to the unbind txn.
    txn.Reply();
  }
}

void AmlUartV1::DdkRelease() {
  if (aml_uart_.has_value()) {
    aml_uart_->SerialImplAsyncEnable(false);
  }

  delete this;
}

zx_status_t AmlUartV1::DdkGetProtocol(uint32_t proto_id, void* out) {
  if (proto_id != ZX_PROTOCOL_SERIAL_IMPL_ASYNC) {
    return ZX_ERR_PROTOCOL_NOT_SUPPORTED;
  }

  if (!aml_uart_.has_value()) {
    return ZX_ERR_NOT_FOUND;
  }

  serial_impl_async_protocol_t* hci_proto = static_cast<serial_impl_async_protocol_t*>(out);
  hci_proto->ops = static_cast<const serial_impl_async_protocol_ops_t*>(aml_uart_->get_ops());
  hci_proto->ctx = &aml_uart_;
  return ZX_OK;
}

zx_status_t AmlUartV1::Init(ddk::PDevFidl pdev, const serial_port_info_t& serial_port_info,
                            fdf::MmioBuffer mmio) {
  zx::result irq_dispatcher_result =
      fdf::SynchronizedDispatcher::Create({}, "aml_uart_irq", [this](fdf_dispatcher_t*) {
        if (unbind_txn_.has_value()) {
          ddk::UnbindTxn txn = std::move(unbind_txn_.value());
          unbind_txn_.reset();
          txn.Reply();
        }
      });
  if (irq_dispatcher_result.is_error()) {
    zxlogf(ERROR, "%s: Failed to create irq dispatcher: %s", __func__,
           irq_dispatcher_result.status_string());
    return irq_dispatcher_result.error_value();
  }

  irq_dispatcher_.emplace(std::move(irq_dispatcher_result.value()));
  aml_uart_.emplace(std::move(pdev), serial_port_info, std::move(mmio), irq_dispatcher_->borrow());

  auto cleanup = fit::defer([this]() { DdkRelease(); });

  // Default configuration for the case that serial_impl_config is not called.
  constexpr uint32_t kDefaultBaudRate = 115200;
  constexpr uint32_t kDefaultConfig = SERIAL_DATA_BITS_8 | SERIAL_STOP_BITS_1 | SERIAL_PARITY_NONE;
  aml_uart_->SerialImplAsyncConfig(kDefaultBaudRate, kDefaultConfig);
  zx_device_prop_t props[] = {
      {BIND_PROTOCOL, 0, ZX_PROTOCOL_SERIAL_IMPL_ASYNC},
      {BIND_SERIAL_CLASS, 0, aml_uart_->serial_port_info().serial_class},
  };

  fuchsia_hardware_serialimpl::Service::InstanceHandler handler({
      .device =
          [this](fdf::ServerEnd<fuchsia_hardware_serialimpl::Device> server_end) {
            serial_impl_bindings_.AddBinding(fdf::Dispatcher::GetCurrent()->get(),
                                             std::move(server_end), &aml_uart_.value(),
                                             fidl::kIgnoreBindingClosure);
          },
  });

  zx::result<> add_result =
      outgoing_.AddService<fuchsia_hardware_serialimpl::Service>(std::move(handler));
  if (add_result.is_error()) {
    zxlogf(ERROR, "Failed to add fuchsia_hardware_serialimpl::Service %s",
           add_result.status_string());
    return add_result.status_value();
  }

  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    zxlogf(ERROR, "failed to create endpoints: %s\n", endpoints.status_string());
    return endpoints.status_value();
  }

  auto result = outgoing_.Serve(std::move(endpoints->server));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to serve outgoing directory: %s\n", result.status_string());
    return result.error_value();
  }

  std::array offers = {
      fuchsia_hardware_serialimpl::Service::Name,
  };

  auto status = DdkAdd(ddk::DeviceAddArgs("aml-uart")
                           .set_props(props)
                           .set_runtime_service_offers(offers)
                           .set_outgoing_dir(endpoints->client.TakeChannel())
                           .forward_metadata(parent(), DEVICE_METADATA_MAC_ADDRESS));
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: DdkDeviceAdd failed", __func__);
    return status;
  }

  cleanup.cancel();
  return status;
}

static constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = AmlUartV1::Create;
  return ops;
}();

}  // namespace serial

ZIRCON_DRIVER(aml_uart, serial::driver_ops, "zircon", "0.1");
