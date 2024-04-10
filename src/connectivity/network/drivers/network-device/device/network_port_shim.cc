// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "network_port_shim.h"

#include "mac_addr_shim.h"

namespace network {

namespace netdev = fuchsia_hardware_network;

void NetworkPortShim::Bind(ddk::NetworkPortProtocolClient client_impl, fdf_dispatcher_t* dispatcher,
                           fdf::ServerEnd<netdriver::NetworkPort> server_end) {
  std::unique_ptr<NetworkPortShim> impl(new NetworkPortShim(client_impl, dispatcher));

  fdf::BindServer(impl->dispatcher_, std::move(server_end), std::move(impl));
}

NetworkPortShim::NetworkPortShim(ddk::NetworkPortProtocolClient impl, fdf_dispatcher_t* dispatcher)
    : impl_(impl), dispatcher_(dispatcher) {}

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
  builder.port_class(static_cast<netdev::DeviceClass>(info.port_class))
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

  MacAddrShim::Bind(dispatcher_, mac_addr, std::move(endpoints->server));

  completer.buffer(arena).Reply(std::move(endpoints->client));
}

void NetworkPortShim::Removed(fdf::Arena& arena, RemovedCompleter::Sync& completer) {
  impl_.Removed();
}

}  // namespace network
