// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "device.h"

#include <fidl/fuchsia.wlan.device/cpp/wire.h>
#include <fuchsia/wlan/common/cpp/fidl.h>
#include <fuchsia/wlan/internal/cpp/fidl.h>
#include <inttypes.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/driver.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/fidl/cpp/wire/arena.h>
#include <net/ethernet.h>
#include <zircon/status.h>

#include <iterator>

#include <sdk/lib/driver/logging/cpp/logger.h>
#include <wlan/common/band.h>
#include <wlan/common/channel.h>
#include <wlan/common/element.h>
#include <wlan/common/phy.h>

#include "debug.h"
#include "wlan/drivers/log_instance.h"

namespace wlanphy {

namespace wlan_common = ::fuchsia::wlan::common;
namespace wlan_internal = ::fuchsia::wlan::internal;

Device::Device(fdf::DriverStartArgs start_args,
               fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : DriverBase("wlanphy", std::move(start_args), std::move(driver_dispatcher)),
      devfs_connector_(fit::bind_member<&Device::Serve>(this)) {
  wlan::drivers::log::Instance::Init(0);

  auto client_dispatcher =
      fdf::SynchronizedDispatcher::Create({}, "wlanphy", [&](fdf_dispatcher_t*) {});

  ZX_ASSERT_MSG(!client_dispatcher.is_error(), "Creating dispatcher error: %s",
                zx_status_get_string(client_dispatcher.status_value()));
  client_dispatcher_ = std::move(*client_dispatcher);
}

zx::result<> Device::Start() {
  // Initialize our device server.
  {
    zx::result<> result = compat_server_.Initialize(incoming(), outgoing(), node_name(), name(),
                                                    compat::ForwardMetadata::None());
    if (result.is_error()) {
      return result.take_error();
    }
  }

  zx_status_t status;
  if ((status = ConnectToWlanPhyImpl()) != ZX_OK) {
    lerror("Connect to WlanPhyImpl failed: %s", zx_status_get_string(status));
    return zx::error(status);
  }
  if ((status = AddWlanDeviceConnector()) != ZX_OK) {
    lerror("Adding WlanPhy service failed: %s", zx_status_get_string(status));
    return zx::error(status);
  }
  return zx::ok();
}

void Device::PrepareStop(fdf::PrepareStopCompleter completer) {
  client_.AsyncTeardown();
  completer(zx::ok());
}

void Device::Connect(ConnectRequestView request, ConnectCompleter::Sync& completer) {
  ConnectPhyServerEnd(std::move(request->request));
}

void Device::ConnectPhyServerEnd(fidl::ServerEnd<fuchsia_wlan_device::Phy> server_end) {
  ltrace_fn();
  phy_bindings_.AddBinding(dispatcher(), std::move(server_end), this, fidl::kIgnoreBindingClosure);
}

zx_status_t Device::ConnectToWlanPhyImpl() {
  auto client_end = incoming()->Connect<fuchsia_wlan_phyimpl::Service::WlanPhyImpl>();
  if (client_end.is_error()) {
    lerror("Connect to wlanphyimpl service Failed = %s", client_end.status_string());
    return client_end.status_value();
  }
  client_.Bind(std::move(*client_end), client_dispatcher_.get());
  if (!client_.is_valid()) {
    lerror("WlanPhyImpl Client is not valid");
    return ZX_ERR_BAD_HANDLE;
  }
  return ZX_OK;
}

zx_status_t Device::AddWlanDeviceConnector() {
  fidl::Arena arena;
  zx::result connector = devfs_connector_.Bind(dispatcher());
  if (connector.is_error()) {
    return connector.status_value();
  }

  auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena)
                   .connector(std::move(connector.value()))
                   .class_name("wlanphy");

  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                  .name(arena, "wlanphy")
                  .devfs_args(devfs.Build())
                  .Build();
  // Create endpoints of the `NodeController` for the node.
  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (!controller_endpoints.is_ok()) {
    lerror("Failed to create endpoints: %s", controller_endpoints.status_string());
    return controller_endpoints.status_value();
  }

  zx::result node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
  if (!node_endpoints.is_ok()) {
    lerror("Failed to create endpoints: %s", node_endpoints.status_string());
    return node_endpoints.status_value();
  }

  fidl::WireResult result = fidl::WireCall(node())->AddChild(
      args, std::move(controller_endpoints->server), std::move(node_endpoints->server));
  if (!result.ok()) {
    lerror("Failed to add child, status: %s", result.status_string());
    return result.status();
  }
  controller_node_.Bind(std::move(controller_endpoints->client));
  node_.Bind(std::move(node_endpoints->client));
  return ZX_OK;
}

void Device::GetSupportedMacRoles(GetSupportedMacRolesCompleter::Sync& completer) {
  ltrace_fn();
  constexpr uint32_t kTag = 'GSMC';
  fdf::Arena fdf_arena(kTag);

  client_.buffer(fdf_arena)->GetSupportedMacRoles().ThenExactlyOnce(
      [completer = completer.ToAsync()](
          fdf::WireUnownedResult<fuchsia_wlan_phyimpl::WlanPhyImpl::GetSupportedMacRoles>&
              result) mutable {
        if (!result.ok()) {
          completer.ReplyError(result.status());
          return;
        }
        if (result->is_error()) {
          completer.ReplyError(result->error_value());
          return;
        }
        if (!result->value()->has_supported_mac_roles()) {
          completer.ReplyError(ZX_ERR_UNAVAILABLE);
        }
        if (result->value()->supported_mac_roles().count() >
            fuchsia_wlan_common_MAX_SUPPORTED_MAC_ROLES) {
          completer.ReplyError(ZX_ERR_OUT_OF_RANGE);
          return;
        }

        completer.ReplySuccess(result->value()->supported_mac_roles());
      });
}

const fidl::Array<uint8_t, 6> NULL_MAC_ADDR{0x00, 0x00, 0x00, 0x00, 0x00, 0x00};

void Device::CreateIface(CreateIfaceRequestView request, CreateIfaceCompleter::Sync& completer) {
  ltrace_fn();
  constexpr uint32_t kTag = 'CIFC';
  fdf::Arena fdf_arena(kTag);

  fidl::Arena fidl_arena;
  auto builder = fuchsia_wlan_phyimpl::wire::WlanPhyImplCreateIfaceRequest::Builder(fidl_arena);
  builder.role(request->req.role);
  builder.mlme_channel(std::move(request->req.mlme_channel));

  if (!std::equal(std::begin(NULL_MAC_ADDR), std::end(NULL_MAC_ADDR),
                  request->req.init_sta_addr.data())) {
    builder.init_sta_addr(request->req.init_sta_addr);
  }

  client_.buffer(fdf_arena)
      ->CreateIface(builder.Build())
      .ThenExactlyOnce([completer = completer.ToAsync()](
                           fdf::WireUnownedResult<fuchsia_wlan_phyimpl::WlanPhyImpl::CreateIface>&
                               result) mutable {
        if (!result.ok()) {
          lerror("CreateIface failed with FIDL error %s", result.status_string());
          completer.ReplyError(result.status());
          return;
        }
        if (result->is_error()) {
          lerror("CreateIface failed with error %s", zx_status_get_string(result->error_value()));
          completer.ReplyError(result->error_value());
          return;
        }

        if (!result->value()->has_iface_id()) {
          lerror("CreateIface failed. Response missing iface_id");
          completer.ReplyError(ZX_ERR_INTERNAL);
          return;
        }
        completer.ReplySuccess(result->value()->iface_id());
      });
}

void Device::DestroyIface(DestroyIfaceRequestView request, DestroyIfaceCompleter::Sync& completer) {
  ltrace_fn();
  constexpr uint32_t kTag = 'DIFC';
  fdf::Arena fdf_arena(kTag);

  fidl::Arena fidl_arena;
  auto builder = fuchsia_wlan_phyimpl::wire::WlanPhyImplDestroyIfaceRequest::Builder(fidl_arena);
  builder.iface_id(request->req.id);

  client_.buffer(fdf_arena)
      ->DestroyIface(builder.Build())
      .ThenExactlyOnce([completer = completer.ToAsync()](
                           fdf::WireUnownedResult<fuchsia_wlan_phyimpl::WlanPhyImpl::DestroyIface>&
                               result) mutable {
        if (!result.ok()) {
          lerror("DestroyIface failed with FIDL error %s", result.status_string());
          completer.ReplyError(result.status());
          return;
        }
        if (result->is_error()) {
          if (result->error_value() != ZX_ERR_NOT_FOUND) {
            lerror("DestroyIface failed with error %s",
                   zx_status_get_string(result->error_value()));
          }
          completer.ReplyError(result->error_value());
          return;
        }

        completer.ReplySuccess();
      });
}

void Device::SetCountry(SetCountryRequestView request, SetCountryCompleter::Sync& completer) {
  ltrace_fn();
  ldebug_device("SetCountry to %s", wlan::common::Alpha2ToStr(request->req.alpha2).c_str());
  constexpr uint32_t kTag = 'SCNT';
  fdf::Arena fdf_arena(kTag);

  auto alpha2 = ::fidl::Array<uint8_t, fuchsia_wlan_phyimpl::wire::kWlanphyAlpha2Len>();
  memcpy(alpha2.data(), request->req.alpha2.data(), fuchsia_wlan_phyimpl::wire::kWlanphyAlpha2Len);

  auto out_country = fuchsia_wlan_phyimpl::wire::WlanPhyCountry::WithAlpha2(alpha2);
  client_.buffer(fdf_arena)
      ->SetCountry(out_country)
      .ThenExactlyOnce([completer = completer.ToAsync()](
                           fdf::WireUnownedResult<fuchsia_wlan_phyimpl::WlanPhyImpl::SetCountry>&
                               result) mutable {
        if (!result.ok()) {
          lerror("SetCountry failed with FIDL error %s", result.status_string());
          completer.Reply(result.status());
          return;
        }
        if (result->is_error()) {
          lerror("SetCountry failed with error %s", zx_status_get_string(result->error_value()));
          completer.Reply(result->error_value());
          return;
        }

        completer.Reply(ZX_OK);
      });
}

void Device::GetCountry(GetCountryCompleter::Sync& completer) {
  ltrace_fn();
  constexpr uint32_t kTag = 'GCNT';
  fdf::Arena fdf_arena(kTag);

  client_.buffer(fdf_arena)->GetCountry().ThenExactlyOnce(
      [completer = completer.ToAsync()](
          fdf::WireUnownedResult<fuchsia_wlan_phyimpl::WlanPhyImpl::GetCountry>& result) mutable {
        fuchsia_wlan_device::wire::CountryCode resp;
        zx_status_t status;
        if (!result.ok()) {
          lerror("GetCountry failed with FIDL error %s", result.status_string());
          status = result.status();
          completer.ReplyError(status);
          return;
        }
        if (result->is_error()) {
          lerror("GetCountry failed with error %s", zx_status_get_string(result->error_value()));
          status = result->error_value();
          completer.ReplyError(status);
          return;
        }
        if (!result->value()->is_alpha2()) {
          lerror("GetCountry failed. Response union is not an alpha2: %" PRIu64,
                 result->value()->Which());
          completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
          return;
        }
        memcpy(resp.alpha2.data(), result->value()->alpha2().data(),
               fuchsia_wlan_phyimpl::wire::kWlanphyAlpha2Len);

        completer.ReplySuccess(resp);
      });
}

void Device::ClearCountry(ClearCountryCompleter::Sync& completer) {
  ltrace_fn();
  constexpr uint32_t kTag = 'CCNT';
  fdf::Arena fdf_arena(kTag);

  client_.buffer(fdf_arena)->ClearCountry().ThenExactlyOnce(
      [completer = completer.ToAsync()](
          fdf::WireUnownedResult<fuchsia_wlan_phyimpl::WlanPhyImpl::ClearCountry>& result) mutable {
        if (!result.ok()) {
          lerror("ClearCountry failed with FIDL error %s", result.status_string());
          completer.Reply(result.status());
          return;
        }
        if (result->is_error()) {
          lerror("ClearCountry failed with error %s", zx_status_get_string(result->error_value()));
          completer.Reply(result->error_value());
          return;
        }

        completer.Reply(ZX_OK);
      });
}

void Device::SetPowerSaveMode(SetPowerSaveModeRequestView request,
                              SetPowerSaveModeCompleter::Sync& completer) {
  ltrace_fn();
  ldebug_device("SetPowerSaveMode to %d", request->req);
  constexpr uint32_t kTag = 'SPSM';
  fdf::Arena fdf_arena(kTag);

  fidl::Arena fidl_arena;
  auto builder =
      fuchsia_wlan_phyimpl::wire::WlanPhyImplSetPowerSaveModeRequest::Builder(fidl_arena);
  builder.ps_mode(request->req);

  client_.buffer(fdf_arena)
      ->SetPowerSaveMode(builder.Build())
      .ThenExactlyOnce(
          [completer = completer.ToAsync()](
              fdf::WireUnownedResult<fuchsia_wlan_phyimpl::WlanPhyImpl::SetPowerSaveMode>&
                  result) mutable {
            if (!result.ok()) {
              lerror("SetPowerSaveMode failed with FIDL error %s", result.status_string());
              completer.Reply(result.status());
              return;
            }
            if (result->is_error()) {
              lerror("SetPowerSaveMode failed with error %s",
                     zx_status_get_string(result->error_value()));
              completer.Reply(result->error_value());
              return;
            }

            completer.Reply(ZX_OK);
          });
}

void Device::GetPowerSaveMode(GetPowerSaveModeCompleter::Sync& completer) {
  ltrace_fn();
  constexpr uint32_t kTag = 'GPSM';
  fdf::Arena fdf_arena(kTag);

  client_.buffer(fdf_arena)->GetPowerSaveMode().ThenExactlyOnce(
      [completer = completer.ToAsync()](
          fdf::WireUnownedResult<fuchsia_wlan_phyimpl::WlanPhyImpl::GetPowerSaveMode>&
              result) mutable {
        if (!result.ok()) {
          lerror("GetPowerSaveMode failed with FIDL error %s", result.status_string());
          completer.ReplyError(result.status());
          return;
        }
        if (result->is_error()) {
          lerror("GetPowerSaveMode failed with error %s",
                 zx_status_get_string(result->error_value()));
          completer.ReplyError(result->error_value());
          return;
        }

        if (!result->value()->has_ps_mode()) {
          lerror("GetPowerSaveMode failed. Response missing ps_mode.");
          completer.ReplyError(ZX_ERR_INTERNAL);
          return;
        }

        completer.ReplySuccess(result->value()->ps_mode());
      });
}
}  // namespace wlanphy
FUCHSIA_DRIVER_EXPORT(::wlanphy::Device);
