// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/serial/drivers/aml-uart/aml-uart-dfv1.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <lib/fit/defer.h>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/serial/cpp/bind.h>
#include <ddktl/metadata.h>
#include <fbl/alloc_checker.h>

namespace serial {

zx_status_t AmlUartV1::Create(void* ctx, zx_device_t* parent) {
  fdf::PDev pdev;
  {
    zx::result result =
        DdkConnectFragmentFidlProtocol<fuchsia_hardware_platform_device::Service::Device>(parent,
                                                                                          "pdev");
    if (result.is_error()) {
      zxlogf(ERROR, "Failed to connect to pdev: %s", result.status_string());
      return result.status_value();
    }
    pdev = fdf::PDev{std::move(result.value())};
  }

  zx::result info = ddk::GetEncodedMetadata<fuchsia_hardware_serial::wire::SerialPortInfo>(
      parent, DEVICE_METADATA_SERIAL_PORT_INFO);
  if (info.is_error()) {
    zxlogf(ERROR, "device_get_metadata failed: %s", info.status_string());
    return info.error_value();
  }

  zx::result mmio = pdev.MapMmio(0);
  if (mmio.is_error()) {
    zxlogf(ERROR, "Failed to map mmio: %s", mmio.status_string());
    return mmio.status_value();
  }

  fbl::AllocChecker ac;
  auto* uart = new (&ac) AmlUartV1(parent);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  return uart->Init(std::move(pdev), **info, std::move(mmio.value()));
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
    aml_uart_->Enable(false);
  }

  delete this;
}

zx_status_t AmlUartV1::Init(fdf::PDev pdev,
                            const fuchsia_hardware_serial::wire::SerialPortInfo& serial_port_info,
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
  constexpr uint32_t kDefaultConfig = fuchsia_hardware_serialimpl::kSerialDataBits8 |
                                      fuchsia_hardware_serialimpl::kSerialStopBits1 |
                                      fuchsia_hardware_serialimpl::kSerialParityNone;
  aml_uart_->Config(kDefaultBaudRate, kDefaultConfig);
  zx_device_str_prop_t props[] = {
      ddk::MakeStrProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_serial::BIND_PROTOCOL_IMPL_ASYNC),
      ddk::MakeStrProperty(bind_fuchsia::SERIAL_CLASS,
                           static_cast<uint32_t>(aml_uart_->serial_port_info().serial_class)),
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
                           .set_str_props(props)
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
