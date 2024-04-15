// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_DEVICE_NETWORK_PORT_SHIM_H_
#define SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_DEVICE_NETWORK_PORT_SHIM_H_

#include <fidl/fuchsia.hardware.network.driver/cpp/driver/fidl.h>
#include <fuchsia/hardware/network/driver/cpp/banjo.h>

#include <fbl/intrusive_double_list.h>

#include "mac_addr_shim.h"

namespace network {

namespace netdriver = fuchsia_hardware_network_driver;

// Translates calls between the parent device and the underlying netdevice.
//
// Usage of this type assumes that the parent device speaks Banjo while the underlying netdevice
// port speaks FIDL. This type translates calls from from netdevice into the parent from FIDL to
// Banjo. The NetworkPort protocol does not have corresponding Ifc protocol in the other direction
// so this type only needs to work in one direction.
class NetworkPortShim : public fdf::WireServer<netdriver::NetworkPort>,
                        public fbl::DoublyLinkedListable<std::unique_ptr<NetworkPortShim>> {
 public:
  NetworkPortShim(ddk::NetworkPortProtocolClient impl, fdf_dispatcher_t* dispatcher,
                  fdf::ServerEnd<netdriver::NetworkPort> server_end,
                  fit::callback<void(NetworkPortShim*)>&& on_unbound);

  // NetworkPort implementation
  void GetInfo(fdf::Arena& arena, GetInfoCompleter::Sync& completer) override;
  void GetStatus(fdf::Arena& arena, GetStatusCompleter::Sync& completer) override;
  void SetActive(netdriver::wire::NetworkPortSetActiveRequest* request, fdf::Arena& arena,
                 SetActiveCompleter::Sync& completer) override;
  void GetMac(fdf::Arena& arena, GetMacCompleter::Sync& completer) override;
  void Removed(fdf::Arena& arena, RemovedCompleter::Sync& completer) override;

 private:
  using MacAddrShimList = fbl::SizedDoublyLinkedList<std::unique_ptr<MacAddrShim>>;

  void OnPortUnbound(fidl::UnbindInfo info);

  ddk::NetworkPortProtocolClient impl_;
  fdf_dispatcher_t* dispatcher_;
  // All access to this list must by design happen on the above dispatcher. MacAddrShim objects can
  // only be constructed on the same dispatcher they are bound and unbound on.
  MacAddrShimList mac_addr_shims_;
  fit::callback<void(NetworkPortShim*)> on_unbound_;
  fdf::ServerBinding<netdriver::NetworkPort> binding_;
};

}  // namespace network

#endif  // SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_DEVICE_NETWORK_PORT_SHIM_H_
