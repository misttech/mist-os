// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/lib/usb-phy/include/usb-phy/usb-phy.h"

#include <lib/ddk/debug.h>

#include <ddktl/device.h>

namespace usb_phy {

zx::result<UsbPhyClient> UsbPhyClient::Create(zx_device_t* parent,
                                              const std::string_view fragment_name) {
  // Prefer Banjo over FIDL.
  auto phy = ddk::UsbPhyProtocolClient(parent, fragment_name.data());
  if (phy.is_valid()) {
    return zx::ok(UsbPhyClient(phy));
  }

  // Try to get FIDL.
  auto client_end = ddk::Device<void>::DdkConnectFragmentRuntimeProtocol<
      fuchsia_hardware_usb_phy::Service::Device>(parent, fragment_name.data());
  if (client_end.is_ok()) {
    return zx::ok(UsbPhyClient(std::move(client_end.value())));
  }

  zxlogf(ERROR, "Could not get either Banjo or FIDL client");
  return zx::error(ZX_ERR_NOT_FOUND);
}

zx::result<UsbPhyClient> UsbPhyClient::Create(zx_device_t* parent) {
  // Prefer Banjo over FIDL.
  auto phy = ddk::UsbPhyProtocolClient(parent);
  if (phy.is_valid()) {
    return zx::ok(UsbPhyClient(phy));
  }

  // Try to get FIDL.
  auto client_end =
      ddk::Device<void>::DdkConnectRuntimeProtocol<fuchsia_hardware_usb_phy::Service::Device>(
          parent);
  if (client_end.is_ok()) {
    return zx::ok(UsbPhyClient(std::move(client_end.value())));
  }

  zxlogf(ERROR, "Could not get either Banjo or FIDL client");
  return zx::error(ZX_ERR_NOT_FOUND);
}

zx_status_t UsbPhyClient::ConnectStatusChanged(bool connected) {
  if (std::holds_alternative<BanjoClient>(client_)) {
    std::get<BanjoClient>(client_).ConnectStatusChanged(connected);
    return ZX_OK;
  }

  fdf::Arena arena('UPHY');
  auto result = std::get<FidlClient>(client_).buffer(arena)->ConnectStatusChanged(connected);
  if (!result.ok()) {
    zxlogf(ERROR, "ConnectStatusChanged failed %s", result.FormatDescription().c_str());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "ConnectStatusChanged failed %d", result->error_value());
    return result->error_value();
  }
  return ZX_OK;
}

}  // namespace usb_phy
