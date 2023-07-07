// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "wlantap-ctl.h"

#include <fidl/fuchsia.wlan.tap/cpp/fidl.h>
#include <fidl/fuchsia.wlan.tap/cpp/wire.h>
#include <lib/ddk/driver.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <bind/fuchsia/wlan/phyimpl/cpp/bind.h>

#include "wlantap-phy.h"

namespace wlan {

void WlantapCtlServer::CreatePhy(CreatePhyRequestView request,
                                 CreatePhyCompleter::Sync& completer) {
  phy_config_arena_.Reset();

  auto natural_config = fidl::ToNatural(request->config);
  auto wire_config = fidl::ToWire(phy_config_arena_, std::move(natural_config));
  auto phy_config = std::make_shared<wlan_tap::WlantapPhyConfig>(wire_config);

  std::string_view instance_name = phy_config->name.get();

  auto endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (endpoints.is_error()) {
    FDF_LOG(ERROR, "Failed to create endpoints: %s", endpoints.status_string());
    completer.Reply(endpoints.error_value());
  }

  auto impl = std::make_unique<WlanPhyImplDevice>(driver_context_, request->proxy.TakeChannel(),
                                                  phy_config, std::move(endpoints->client));
  zx_status_t status = ServeWlanPhyImplProtocol(instance_name, std::move(impl));
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "ServeWlanPhyImplProtocol failed: %s", zx_status_get_string(status));
    completer.Reply(status);
    return;
  }

  status = AddWlanPhyImplChild(instance_name, std::move(endpoints->server));
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "AddWlanPhyImplChild failed: %s", zx_status_get_string(status));
    completer.Reply(status);
    return;
  }

  completer.Reply(ZX_OK);
}

zx_status_t WlantapCtlServer::AddWlanPhyImplChild(
    std::string_view name, fidl::ServerEnd<fuchsia_driver_framework::NodeController> server) {
  fidl::Arena arena;

  auto offers = std::vector{fdf::MakeOffer<fuchsia_wlan_phyimpl::Service>(arena, name)};

  auto properties = fidl::VectorView<fuchsia_driver_framework::wire::NodeProperty>(arena, 1);
  properties[0] = fdf::MakeProperty(arena, bind_fuchsia_wlan_phyimpl::WLANPHYIMPL,
                                    bind_fuchsia_wlan_phyimpl::WLANPHYIMPL_DRIVERTRANSPORT);

  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                  .name(name)
                  .properties(properties)
                  .offers(offers)
                  .Build();

  auto res = driver_context_.node_client()->AddChild(args, std::move(server), {});
  if (!res.ok()) {
    FDF_LOG(ERROR, "Failed to add WlanPhyImpl child: %s", res.status_string());
    return res.status();
  }
  return ZX_OK;
}

zx_status_t WlantapCtlServer::ServeWlanPhyImplProtocol(std::string_view name,
                                                       std::unique_ptr<WlanPhyImplDevice> impl) {
  auto protocol_handler =
      [impl = std::move(impl)](fdf::ServerEnd<fuchsia_wlan_phyimpl::WlanPhyImpl> request) mutable {
        fdf::BindServer(fdf::Dispatcher::GetCurrent()->get(), std::move(request), std::move(impl));
      };

  fuchsia_wlan_phyimpl::Service::InstanceHandler handler(
      {.wlan_phy_impl = std::move(protocol_handler)});

  zx::result result = driver_context_.outgoing()->AddService<fuchsia_wlan_phyimpl::Service>(
      std::move(handler), name);

  if (result.is_error()) {
    FDF_LOG(ERROR, "Failed to add service: %s", result.status_string());
    return result.error_value();
  }

  return ZX_OK;
}

}  // namespace wlan
