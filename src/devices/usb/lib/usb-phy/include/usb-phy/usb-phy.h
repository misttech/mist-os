// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_LIB_USB_PHY_INCLUDE_USB_PHY_USB_PHY_H_
#define SRC_DEVICES_USB_LIB_USB_PHY_INCLUDE_USB_PHY_USB_PHY_H_

#include <fidl/fuchsia.hardware.usb.phy/cpp/driver/wire.h>
#include <fuchsia/hardware/usb/phy/cpp/banjo.h>
#include <lib/driver/incoming/cpp/namespace.h>

#include <variant>

namespace usb_phy {

class UsbPhyClient {
 public:
  static zx::result<UsbPhyClient> Create(const std::shared_ptr<fdf::Namespace>& incoming,
                                         const std::string_view fragment_name);
  static zx::result<UsbPhyClient> Create(const std::shared_ptr<fdf::Namespace>& incoming);
  static zx::result<UsbPhyClient> Create(zx_device_t* parent, const std::string_view fragment_name);
  static zx::result<UsbPhyClient> Create(zx_device_t* parent);

  zx_status_t ConnectStatusChanged(bool connected);

 private:
  // FIDL Constructor.
  explicit UsbPhyClient(fdf::ClientEnd<fuchsia_hardware_usb_phy::UsbPhy> client)
      : client_(FidlClient(std::move(client))) {}
  // Banjo Constructor.
  explicit UsbPhyClient(ddk::UsbPhyProtocolClient client) : client_(client) {}

  using FidlClient = fdf::WireSyncClient<fuchsia_hardware_usb_phy::UsbPhy>;
  using BanjoClient = ddk::UsbPhyProtocolClient;

  std::variant<FidlClient, BanjoClient> client_;
};

}  // namespace usb_phy

#endif  // SRC_DEVICES_USB_LIB_USB_PHY_INCLUDE_USB_PHY_USB_PHY_H_
