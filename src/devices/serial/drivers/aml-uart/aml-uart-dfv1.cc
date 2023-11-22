// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/serial/drivers/aml-uart/aml-uart-dfv1.h"

#include <fuchsia/hardware/serial/c/banjo.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/metadata.h>
#include <lib/fit/defer.h>

#include <fbl/alloc_checker.h>

namespace serial {

zx_status_t AmlUartV1::Create(void* ctx, zx_device_t* parent) {
  zx_status_t status;
  auto pdev = ddk::PDevFidl::FromFragment(parent);
  if (!pdev.is_valid()) {
    zxlogf(ERROR, "AmlUart::Create: Could not get pdev");
    return ZX_ERR_NO_RESOURCES;
  }

  serial_port_info_t info;
  size_t actual;
  status =
      device_get_metadata(parent, DEVICE_METADATA_SERIAL_PORT_INFO, &info, sizeof(info), &actual);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: device_get_metadata failed %d", __func__, status);
    return status;
  }
  if (actual < sizeof(info)) {
    zxlogf(ERROR, "%s: serial_port_info_t metadata too small", __func__);
    return ZX_ERR_INTERNAL;
  }

  std::optional<fdf::MmioBuffer> mmio;
  status = pdev.MapMmio(0, &mmio);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: pdev_map_&mmio__buffer failed %d", __func__, status);
    return status;
  }

  fbl::AllocChecker ac;
  auto* uart = new (&ac) AmlUartV1(parent, std::move(pdev), info, *std::move(mmio));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  return uart->Init();
}

void AmlUartV1::DdkRelease() {
  aml_uart_.SerialImplAsyncEnable(false);
  delete this;
}

zx_status_t AmlUartV1::DdkGetProtocol(uint32_t proto_id, void* out) {
  if (proto_id != ZX_PROTOCOL_SERIAL_IMPL_ASYNC) {
    return ZX_ERR_PROTOCOL_NOT_SUPPORTED;
  }

  serial_impl_async_protocol_t* hci_proto = static_cast<serial_impl_async_protocol_t*>(out);
  hci_proto->ops = static_cast<const serial_impl_async_protocol_ops_t*>(aml_uart_.get_ops());
  hci_proto->ctx = &aml_uart_;
  return ZX_OK;
}

zx_status_t AmlUartV1::Init() {
  auto cleanup = fit::defer([this]() { DdkRelease(); });

  // Default configuration for the case that serial_impl_config is not called.
  constexpr uint32_t kDefaultBaudRate = 115200;
  constexpr uint32_t kDefaultConfig = SERIAL_DATA_BITS_8 | SERIAL_STOP_BITS_1 | SERIAL_PARITY_NONE;
  aml_uart_.SerialImplAsyncConfig(kDefaultBaudRate, kDefaultConfig);
  zx_device_prop_t props[] = {
      {BIND_PROTOCOL, 0, ZX_PROTOCOL_SERIAL_IMPL_ASYNC},
      {BIND_SERIAL_CLASS, 0, aml_uart_.serial_port_info().serial_class},
  };
  auto status = DdkAdd(ddk::DeviceAddArgs("aml-uart")
                           .set_props(props)
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
