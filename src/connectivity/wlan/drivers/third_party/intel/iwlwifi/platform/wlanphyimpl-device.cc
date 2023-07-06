// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/platform/wlanphyimpl-device.h"

#include <lib/sync/cpp/completion.h>
#include <zircon/status.h>

#include <memory>

extern "C" {
#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/mvm/mvm.h"
}  // extern "C"

#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/platform/mvm-mlme.h"
#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/platform/wlansoftmac-device.h"

namespace wlan::iwlwifi {

namespace phyimpl_fidl = fuchsia_wlan_phyimpl::wire;

WlanPhyImplDevice::WlanPhyImplDevice() = default;

WlanPhyImplDevice::~WlanPhyImplDevice() = default;

void WlanPhyImplDevice::GetSupportedMacRoles(fdf::Arena& arena,
                                             GetSupportedMacRolesCompleter::Sync& completer) {
  // The fidl array which will be returned to Wlanphy driver.
  fuchsia_wlan_common::WlanMacRole
      supported_mac_roles_list[fuchsia_wlan_common::wire::kMaxSupportedMacRoles] = {};
  uint8_t supported_mac_roles_count = 0;
  zx_status_t status =
      phy_get_supported_mac_roles(drvdata(), supported_mac_roles_list, &supported_mac_roles_count);
  if (status != ZX_OK) {
    IWL_ERR(this, "failed get supported mac roles: %s", zx_status_get_string(status));
    completer.buffer(arena).ReplyError(status);
    return;
  }

  if (supported_mac_roles_count > fuchsia_wlan_common::wire::kMaxSupportedMacRoles) {
    IWL_ERR(this,
            "Too many mac roles returned from iwlwifi driver. Number of supported mac roles got "
            "from driver is %u, but the limitation is: %u\n",
            supported_mac_roles_count, fuchsia_wlan_common::wire::kMaxSupportedMacRoles);
    completer.buffer(arena).ReplyError(ZX_ERR_OUT_OF_RANGE);
    return;
  }

  auto reply_vector = fidl::VectorView<fuchsia_wlan_common::WlanMacRole>::FromExternal(
      supported_mac_roles_list, supported_mac_roles_count);

  fidl::Arena fidl_arena;
  auto builder =
      fuchsia_wlan_phyimpl::wire::WlanPhyImplGetSupportedMacRolesResponse::Builder(fidl_arena);
  builder.supported_mac_roles(reply_vector);
  completer.buffer(arena).ReplySuccess(builder.Build());
}

void WlanPhyImplDevice::CreateIface(CreateIfaceRequestView request, fdf::Arena& arena,
                                    CreateIfaceCompleter::Sync& completer) {
  zx_status_t status = ZX_OK;
  if (!request->has_role() || !request->has_mlme_channel()) {
    IWL_ERR(this, "missing info in request role(%u), channel(%u)", request->has_role(),
            request->has_mlme_channel());
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  uint16_t out_iface_id;
  wlan_phy_impl_create_iface_req_t create_iface_req;

  switch (request->role()) {
    case fuchsia_wlan_common::WlanMacRole::kClient:
      create_iface_req.role = WLAN_MAC_ROLE_CLIENT;
      break;
    case fuchsia_wlan_common::WlanMacRole::kAp:
      create_iface_req.role = WLAN_MAC_ROLE_AP;
      break;
    case fuchsia_wlan_common::WlanMacRole::kMesh:
      create_iface_req.role = WLAN_MAC_ROLE_MESH;
      break;
    default:
      IWL_ERR(this, "Unrecognized WlanMacRole type from the request. Requested role: %u\n",
              static_cast<uint32_t>(request->role()));
      completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
      return;
  }

  create_iface_req.mlme_channel = request->mlme_channel().release();
  if ((status = phy_create_iface(drvdata(), &create_iface_req, &out_iface_id)) != ZX_OK) {
    IWL_ERR(this, "failed phy create: %s", zx_status_get_string(status));
    completer.buffer(arena).ReplyError(status);
    return;
  }

  struct iwl_mvm* mvm = iwl_trans_get_mvm(drvdata());
  struct iwl_mvm_vif* mvmvif = mvm->mvmvif[out_iface_id];

  if ((status = AddWlansoftmacDevice(out_iface_id, mvmvif)) != ZX_OK) {
    IWL_ERR(this, "%s() failed mac device add: %s\n", __func__, zx_status_get_string(status));
    phy_create_iface_undo(drvdata(), out_iface_id);
    completer.buffer(arena).ReplyError(status);
    return;
  }
  IWL_INFO(this, "%s() created iface %u\n", __func__, out_iface_id);

  fidl::Arena fidl_arena;
  auto builder = phyimpl_fidl::WlanPhyImplCreateIfaceResponse::Builder(fidl_arena);
  builder.iface_id(out_iface_id);
  completer.buffer(arena).ReplySuccess(builder.Build());
}

void WlanPhyImplDevice::DestroyIface(DestroyIfaceRequestView request, fdf::Arena& arena,
                                     DestroyIfaceCompleter::Sync& completer) {
  if (!request->has_iface_id()) {
    IWL_ERR(this, "invoked without valid iface id");
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }
  IWL_INFO(this, "destroying iface %u", request->iface_id());
  zx_status_t status = phy_destroy_iface(drvdata(), request->iface_id());
  if (status != ZX_OK) {
    IWL_ERR(this, "failed destroy iface: %s", zx_status_get_string(status));
    completer.buffer(arena).ReplyError(status);
    return;
  }
  if ((status = RemoveWlansoftmacDevice(request->iface_id())) != ZX_OK) {
    IWL_ERR(this, "%s() failed mac device remove: %s\n", __func__, zx_status_get_string(status));
    completer.buffer(arena).ReplyError(status);
    return;
  }
  completer.buffer(arena).ReplySuccess();
}

void WlanPhyImplDevice::SetCountry(SetCountryRequestView request, fdf::Arena& arena,
                                   SetCountryCompleter::Sync& completer) {
  wlan_phy_country_t country;

  if (!request->is_alpha2()) {
    IWL_ERR(this, "only alpha2 format is supported");
    completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  memcpy(&country.alpha2[0], request->alpha2().data(), WLANPHY_ALPHA2_LEN);
  zx_status_t status = phy_set_country(drvdata(), &country);
  if (status != ZX_OK) {
    IWL_ERR(this, "failed set country: %s", zx_status_get_string(status));
    completer.buffer(arena).ReplyError(status);
    return;
  }

  completer.buffer(arena).ReplySuccess();
}

void WlanPhyImplDevice::ClearCountry(fdf::Arena& arena, ClearCountryCompleter::Sync& completer) {
  IWL_ERR(this, "%s() not implemented ...\n", __func__);
  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void WlanPhyImplDevice::GetCountry(fdf::Arena& arena, GetCountryCompleter::Sync& completer) {
  fidl::Array<uint8_t, WLANPHY_ALPHA2_LEN> alpha2;

  // TODO(fxbug.dev/95504): Remove the usage of wlan_phy_country_t inside the iwlwifi driver.
  wlan_phy_country_t country;
  zx_status_t status = phy_get_country(drvdata(), &country);
  if (status != ZX_OK) {
    completer.buffer(arena).ReplyError(status);
    IWL_ERR(this, "failed get country: %s", zx_status_get_string(status));
    return;
  }

  memcpy(alpha2.begin(), country.alpha2, WLANPHY_ALPHA2_LEN);

  auto out_country = fuchsia_wlan_phyimpl::wire::WlanPhyCountry::WithAlpha2(alpha2);
  completer.buffer(arena).ReplySuccess(out_country);
}

void WlanPhyImplDevice::SetPowerSaveMode(SetPowerSaveModeRequestView request, fdf::Arena& arena,
                                         SetPowerSaveModeCompleter::Sync& completer) {
  IWL_ERR(this, "%s() not implemented ...\n", __func__);
  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void WlanPhyImplDevice::GetPowerSaveMode(fdf::Arena& arena,
                                         GetPowerSaveModeCompleter::Sync& completer) {
  IWL_ERR(this, "%s() not implemented ...\n", __func__);
  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void WlanPhyImplDevice::ServiceConnectHandler(
    fdf_dispatcher_t* dispatcher, fdf::ServerEnd<fuchsia_wlan_phyimpl::WlanPhyImpl> server_end) {
  fdf::BindServer(dispatcher, std::move(server_end), this);
}

}  // namespace wlan::iwlwifi
