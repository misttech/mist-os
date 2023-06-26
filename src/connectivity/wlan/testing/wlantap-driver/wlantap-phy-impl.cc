// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "wlantap-phy-impl.h"

#include <lib/ddk/driver.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <wlan/common/phy.h>

#include "utils.h"

namespace wlan {

WlanPhyImplDevice::WlanPhyImplDevice(
    WlantapDriverContext context, zx::channel user_channel,
    std::shared_ptr<wlan_tap::WlantapPhyConfig> phy_config,
    fidl::ClientEnd<fuchsia_driver_framework::NodeController> phy_controller)
    : driver_context_(std::move(context)),
      phy_config_(std::move(phy_config)),
      wlantap_phy_(std::make_unique<WlantapPhy>(std::move(user_channel), phy_config_,
                                                std::move(phy_controller))) {}

void WlanPhyImplDevice::GetSupportedMacRoles(fdf::Arena& arena,
                                             GetSupportedMacRolesCompleter::Sync& completer) {
  // wlantap-phy only supports a single mac role determined by the config
  wlan_common::WlanMacRole supported[1] = {phy_config_->mac_role};
  auto reply_vec = fidl::VectorView<wlan_common::WlanMacRole>::FromExternal(supported, 1);

  FDF_LOG(INFO, "%s: received a 'GetSupportedMacRoles' DDK request. Responding with roles = {%u}",
          name_.c_str(), static_cast<uint32_t>(phy_config_->mac_role));

  auto response =
      fuchsia_wlan_phyimpl::wire::WlanPhyImplGetSupportedMacRolesResponse::Builder(arena)
          .supported_mac_roles(reply_vec)
          .Build();
  completer.buffer(arena).ReplySuccess(response);
}

void WlanPhyImplDevice::CreateIface(CreateIfaceRequestView request, fdf::Arena& arena,
                                    CreateIfaceCompleter::Sync& completer) {
  FDF_LOG(INFO, "%s: received a 'CreateIface' request", name_.c_str());

  std::string role_str = RoleToString(request->role());
  FDF_LOG(INFO, "%s: received a 'CreateIface' for role: %s", name_.c_str(), role_str.c_str());
  if (phy_config_->mac_role != request->role()) {
    FDF_LOG(ERROR, "%s: CreateIface(%s): role not supported", name_.c_str(), role_str.c_str());
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  if (!request->mlme_channel().is_valid()) {
    FDF_LOG(ERROR, "%s: CreateIface(%s): MLME channel in request is invalid", name_.c_str(),
            role_str.c_str());
    completer.buffer(arena).ReplyError(ZX_ERR_IO_INVALID);
    return;
  }

  zx_status_t status = CreateWlanSoftmac(request->role(), std::move(request->mlme_channel()));
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "%s: CreateIface(%s): Could not create softmac: %s", name_.c_str(),
            role_str.c_str(), zx_status_get_string(status));
    completer.buffer(arena).ReplyError(status);
    return;
  }

  fidl::Arena fidl_arena;
  auto resp = fuchsia_wlan_phyimpl::wire::WlanPhyImplCreateIfaceResponse::Builder(fidl_arena)
                  .iface_id(0)
                  .Build();
  completer.buffer(arena).ReplySuccess(resp);
}

void WlanPhyImplDevice::DestroyIface(DestroyIfaceRequestView request, fdf::Arena& arena,
                                     DestroyIfaceCompleter::Sync& completer) {
  FDF_LOG(INFO, "%s: received a 'DestroyIface' DDK request", name_.c_str());
  if (!softmac_controller_.is_valid() || !wlantap_mac_) {
    FDF_LOG(ERROR, "Iface doesn't exist");
    completer.buffer(arena).ReplyError(ZX_ERR_NOT_FOUND);
    return;
  }

  auto status = softmac_controller_->Remove();
  if (!status.ok()) {
    FDF_LOG(ERROR, "Failed to destroy iface: %s", status.status_string());
    completer.buffer(arena).ReplyError(status.status());
    return;
  }

  wlantap_mac_.reset();

  completer.buffer(arena).ReplySuccess();
}

void WlanPhyImplDevice::SetCountry(SetCountryRequestView request, fdf::Arena& arena,
                                   SetCountryCompleter::Sync& completer) {
  FDF_LOG(INFO, "%s: SetCountry() to [%s] received", name_.c_str(),
          wlan::common::Alpha2ToStr(request->alpha2()).c_str());

  wlan_tap::SetCountryArgs args{.alpha2 = request->alpha2()};
  zx_status_t status = wlantap_phy_->SetCountry(args);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "SetCountry() failed: %s", zx_status_get_string(status));
    completer.buffer(arena).ReplyError(status);
    return;
  }
  completer.buffer(arena).ReplySuccess();
}

void WlanPhyImplDevice::ClearCountry(fdf::Arena& arena, ClearCountryCompleter::Sync& completer) {
  FDF_LOG(WARNING, "%s: ClearCountry() not supported", name_.c_str());
  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void WlanPhyImplDevice::GetCountry(fdf::Arena& arena, GetCountryCompleter::Sync& completer) {
  FDF_LOG(WARNING, "%s: GetCountry() not supported", name_.c_str());
  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void WlanPhyImplDevice::SetPowerSaveMode(SetPowerSaveModeRequestView request, fdf::Arena& arena,
                                         SetPowerSaveModeCompleter::Sync& completer) {
  FDF_LOG(WARNING, "%s: SetPowerSaveMode() not supported", name_.c_str());
  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void WlanPhyImplDevice::GetPowerSaveMode(fdf::Arena& arena,
                                         GetPowerSaveModeCompleter::Sync& completer) {
  FDF_LOG(WARNING, "%s: GetPowerSaveMode() not supported", name_.c_str());
  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}

zx_status_t WlanPhyImplDevice::CreateWlanSoftmac(wlan_common::WlanMacRole role,
                                                 zx::channel mlme_channel) {
  static size_t n = 0;
  char name[ZX_MAX_NAME_LEN + 1];
  snprintf(name, sizeof(name), "wlansoftmac-%lu", n++);

  auto endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (endpoints.is_error()) {
    FDF_LOG(ERROR, "Failed to create endpoints: %s", endpoints.status_string());
    return endpoints.status_value();
  }

  zx_status_t status = ServeWlanSoftmac(name, role, std::move(mlme_channel));
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "ServeWlanSoftmac failed: %s", zx_status_get_string(status));
    return status;
  }

  status = AddWlanSoftmacChild(name, std::move(endpoints->server));
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "AddWlanSoftmacChild failed: %s", zx_status_get_string(status));
    return status;
  }

  // Bind softmac controller for DestroyIface()
  softmac_controller_.Bind(std::move(endpoints->client));
  return ZX_OK;
}

zx_status_t WlanPhyImplDevice::AddWlanSoftmacChild(
    std::string_view name, fidl::ServerEnd<fuchsia_driver_framework::NodeController> server) {
  fidl::Arena arena;

  auto offers = std::vector{fdf::MakeOffer<fuchsia_wlan_softmac::Service>(arena, name)};

  FDF_LOG(INFO, "Creating Child node");
  auto properties = fidl::VectorView<fuchsia_driver_framework::wire::NodeProperty>(arena, 1);
  properties[0] = fdf::MakeProperty(arena, 1, ZX_PROTOCOL_WLAN_SOFTMAC);

  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                  .name(name)
                  .properties(properties)
                  .offers(offers)
                  .Build();

  auto endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (endpoints.is_error()) {
    FDF_LOG(ERROR, "Failed to create endpoints: %s", endpoints.status_string());
    return endpoints.status_value();
  }

  auto res = driver_context_.node_client()->AddChild(args, std::move(endpoints->server), {});
  if (!res.ok()) {
    FDF_LOG(ERROR, "failed to add child: %s", res.status_string());
    return res.status();
  }

  return ZX_OK;
}

zx_status_t WlanPhyImplDevice::ServeWlanSoftmac(std::string_view name,
                                                wlan_common::WlanMacRole role,
                                                zx::channel mlme_channel) {
  if (wlantap_mac_) {
    FDF_LOG(ERROR, "Softmac already exists, only one allowed");
    return ZX_ERR_ALREADY_EXISTS;
  }

  wlantap_mac_ =
      std::make_unique<WlantapMac>(wlantap_phy_.get(), role, phy_config_, std::move(mlme_channel));

  FDF_LOG(INFO, "Adding softmac outgoing service");
  fuchsia_wlan_softmac::Service::InstanceHandler handler(
      {.wlan_softmac = wlantap_mac_->ProtocolHandler()});

  zx::result result = driver_context_.outgoing()->AddService<fuchsia_wlan_softmac::Service>(
      std::move(handler), name);

  if (result.is_error()) {
    FDF_LOG(ERROR, "Failed To add WlanSoftmac service: %s", result.status_string());
    return result.status_value();
  }

  return ZX_OK;
}

}  // namespace wlan
