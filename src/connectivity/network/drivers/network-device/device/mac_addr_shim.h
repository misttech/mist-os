// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_DEVICE_MAC_ADDR_SHIM_H_
#define SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_DEVICE_MAC_ADDR_SHIM_H_

#include <fidl/fuchsia.hardware.network.driver/cpp/driver/wire.h>
#include <fuchsia/hardware/network/driver/cpp/banjo.h>

#include <optional>

namespace network {

namespace netdriver = fuchsia_hardware_network_driver;

// Translates calls between the parent device and the underlying netdevice.
//
// Usage of this type assumes that the parent device speaks Banjo while the underlying netdevice
// port speaks FIDL. This type translates calls from from netdevice into the parent from FIDL to
// Banjo. The MacAddr protocol does not have corresponding Ifc protocol in the other direction so
// this type only needs to work in one direction.
class MacAddrShim : public fdf::WireServer<netdriver::MacAddr> {
 public:
  static void Bind(fdf_dispatcher_t* dispatcher, ddk::MacAddrProtocolClient client_impl,
                   fdf::ServerEnd<netdriver::MacAddr> server_end);

  explicit MacAddrShim(ddk::MacAddrProtocolClient impl);

  void SetMode(netdriver::wire::MacAddrSetModeRequest* request, fdf::Arena& arena,
               SetModeCompleter::Sync& completer) override;
  void GetFeatures(fdf::Arena& arena, GetFeaturesCompleter::Sync& completer) override;
  void GetAddress(fdf::Arena& arena, GetAddressCompleter::Sync& completer) override;

 private:
  ddk::MacAddrProtocolClient impl_;
};

}  // namespace network

#endif  // SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_DEVICE_MAC_ADDR_SHIM_H_
