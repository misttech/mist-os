// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "network_port_shim.h"

namespace network {

namespace netdev = fuchsia_hardware_network;

NetworkPortShim::NetworkPortShim(ddk::NetworkPortProtocolClient impl, fdf_dispatcher_t* dispatcher,
                                 fdf::ServerEnd<netdriver::NetworkPort> server_end,
                                 fit::callback<void(NetworkPortShim*)>&& on_unbound)
    : impl_(impl),
      dispatcher_(dispatcher),
      on_unbound_(std::move(on_unbound)),
      binding_(dispatcher, std::move(server_end), this,
               std::mem_fn(&NetworkPortShim::OnPortUnbound)) {}

void NetworkPortShim::GetInfo(fdf::Arena& arena, GetInfoCompleter::Sync& completer) {
  port_base_info_t info;
  impl_.GetInfo(&info);

  fidl::VectorView<netdev::wire::FrameType> rx_types(arena, info.rx_types_count);
  for (size_t i = 0; i < info.rx_types_count; ++i) {
    rx_types[i] = static_cast<netdev::wire::FrameType>(info.rx_types_list[i]);
  }

  fidl::VectorView<netdev::wire::FrameTypeSupport> tx_types(arena, info.tx_types_count);
  for (size_t i = 0; i < info.tx_types_count; ++i) {
    const frame_type_support_t& tx_support = info.tx_types_list[i];
    tx_types[i] = {
        .type = static_cast<netdev::wire::FrameType>(tx_support.type),
        .features = tx_support.features,
        .supported_flags = netdev::wire::TxFlags(tx_support.supported_flags),
    };
  }

  fidl::WireTableBuilder builder = netdev::wire::PortBaseInfo::Builder(arena);
  builder.port_class(static_cast<netdev::PortClass>(info.port_class))
      .tx_types(fidl::ObjectView<decltype(tx_types)>::FromExternal(&tx_types))
      .rx_types(fidl::ObjectView<decltype(rx_types)>::FromExternal(&rx_types));

  completer.buffer(arena).Reply(builder.Build());
}

void NetworkPortShim::GetStatus(fdf::Arena& arena, GetStatusCompleter::Sync& completer) {
  port_status_t status;
  impl_.GetStatus(&status);

  auto builder = netdev::wire::PortStatus::Builder(arena);
  builder.mtu(status.mtu).flags(netdev::wire::StatusFlags(status.flags));

  completer.buffer(arena).Reply(builder.Build());
}

void NetworkPortShim::SetActive(netdriver::wire::NetworkPortSetActiveRequest* request,
                                fdf::Arena& arena, SetActiveCompleter::Sync& completer) {
  impl_.SetActive(request->active);
}

void NetworkPortShim::GetMac(fdf::Arena& arena, GetMacCompleter::Sync& completer) {
  mac_addr_protocol_t* proto = nullptr;
  impl_.GetMac(&proto);
  if (proto == nullptr || proto->ctx == nullptr || proto->ops == nullptr) {
    // Mac protocol not implemented, return empty client end.
    completer.buffer(arena).Reply(::fdf::ClientEnd<netdriver::MacAddr>());
    return;
  }

  ddk::MacAddrProtocolClient mac_addr(proto);

  zx::result endpoints = fdf::CreateEndpoints<netdriver::MacAddr>();
  if (endpoints.is_error()) {
    completer.Close(endpoints.error_value());
    return;
  }

  // MacAddrShim must be created on the same dispatcher that it's served on. Since the GetMac
  // request is already served on that dispatcher it's safe to directly construct it here.
  auto mac_addr_shim = std::make_unique<MacAddrShim>(
      dispatcher_, mac_addr, std::move(endpoints->server),
      [this](MacAddrShim* mac_addr_shim) { mac_addr_shims_.erase(*mac_addr_shim); });
  mac_addr_shims_.push_front(std::move(mac_addr_shim));

  completer.buffer(arena).Reply(std::move(endpoints->client));
}

void NetworkPortShim::Removed(fdf::Arena& arena, RemovedCompleter::Sync& completer) {
  impl_.Removed();
}

void NetworkPortShim::OnPortUnbound(fidl::UnbindInfo info) {
  if (on_unbound_) {
    on_unbound_(this);
  }
}

}  // namespace network
