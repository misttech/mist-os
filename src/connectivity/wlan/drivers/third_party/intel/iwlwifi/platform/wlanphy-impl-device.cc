// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/platform/wlanphy-impl-device.h"

#include <lib/sync/cpp/completion.h>
#include <zircon/status.h>

#include <memory>

extern "C" {
#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/mvm/mvm.h"
}  // extern "C"

#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/platform/mvm-mlme.h"
#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/platform/wlan-softmac-device.h"

namespace wlan::iwlwifi {

namespace phyimpl_fidl = fuchsia_wlan_phyimpl::wire;

WlanPhyImplDevice::WlanPhyImplDevice(zx_device_t* parent)
    : ::ddk::Device<WlanPhyImplDevice, ::ddk::Initializable, ::ddk::Unbindable>(parent),
      outgoing_dir_(fdf::OutgoingDirectory::Create(fdf::Dispatcher::GetCurrent()->get())) {
  auto dispatcher =
      fdf::SynchronizedDispatcher::Create({}, "wlanphy-impl-server", [&](fdf_dispatcher_t*) {
        if (unbind_txn_)
          unbind_txn_->Reply();
      });
  if (dispatcher.is_error()) {
    IWL_ERR(this, "Failed to create dispatcher: %s\n", dispatcher.status_string());
  }
  server_dispatcher_ = std::move(*dispatcher);
  driver_async_dispatcher_ = fdf::Dispatcher::GetCurrent()->async_dispatcher();
}

WlanPhyImplDevice::~WlanPhyImplDevice() = default;

void WlanPhyImplDevice::DdkRelease() { delete this; }

zx_status_t WlanPhyImplDevice::ServeWlanPhyImplProtocol(
    fidl::ServerEnd<fuchsia_io::Directory> server_end) {
  auto protocol = [this](fdf::ServerEnd<fuchsia_wlan_phyimpl::WlanPhyImpl> server_end) mutable {
    if (unbind_txn_) {
      IWL_WARN(this, "Unbind has started, skip the FIDL server binding.");
      return;
    }
    fdf::BindServer(server_dispatcher_.get(), std::move(server_end), this);
  };
  fuchsia_wlan_phyimpl::Service::InstanceHandler handler({.wlan_phy_impl = std::move(protocol)});
  auto status = outgoing_dir_.AddService<fuchsia_wlan_phyimpl::Service>(std::move(handler));
  if (status.is_error()) {
    IWL_ERR(this, "%s(): Failed to add service to outgoing directory: %s\n", __func__,
            status.status_string());
    return status.error_value();
  }
  auto result = outgoing_dir_.Serve(std::move(server_end));
  if (result.is_error()) {
    IWL_ERR(this, "%s(): Failed to serve outgoing directory: %s\n", __func__,
            result.status_string());
    return result.error_value();
  }

  return ZX_OK;
}

void WlanPhyImplDevice::GetSupportedMacRoles(fdf::Arena& arena,
                                             GetSupportedMacRolesCompleter::Sync& completer) {
  // The fidl array which will be returned to Wlanphy driver.
  fuchsia_wlan_common::WlanMacRole
      supported_mac_roles_list[fuchsia_wlan_common::wire::kMaxSupportedMacRoles] = {};
  uint8_t supported_mac_roles_count = 0;
  zx_status_t status =
      phy_get_supported_mac_roles(drvdata(), supported_mac_roles_list, &supported_mac_roles_count);
  if (status != ZX_OK) {
    IWL_ERR(this, "%s() failed get supported mac roles: %s\n", __func__,
            zx_status_get_string(status));
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
    IWL_ERR(this, "%s() missing info in request role(%u), channel(%u)\n", __func__,
            request->has_role(), request->has_mlme_channel());
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
      IWL_ERR(this, "Unrecognized role from the request. Requested role: %u\n", request->role());
      completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
      return;
  }

  create_iface_req.mlme_channel = request->mlme_channel().release();
  if ((status = phy_create_iface(drvdata(), &create_iface_req, &out_iface_id)) != ZX_OK) {
    IWL_ERR(this, "%s() failed phy create: %s\n", __func__, zx_status_get_string(status));
    completer.buffer(arena).ReplyError(status);
    return;
  }

  struct iwl_mvm* mvm = iwl_trans_get_mvm(drvdata());
  struct iwl_mvm_vif* mvmvif = mvm->mvmvif[out_iface_id];

  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    IWL_ERR(this, "failed to create endpoints: %s\n", endpoints.status_string());
    completer.buffer(arena).ReplyError(endpoints.status_value());
    return;
  }
  std::unique_ptr<WlanSoftmacDevice> wlan_softmac_device;

  libsync::Completion served;
  async::PostTask(driver_async_dispatcher_, [&]() {
    wlan_softmac_device =
        std::make_unique<WlanSoftmacDevice>(parent(), drvdata(), out_iface_id, mvmvif);
    zx_status_t status =
        wlan_softmac_device->ServeWlanSoftmacProtocol(std::move(endpoints->server));
    if (status != ZX_OK) {
      IWL_ERR(this, "failed to serve wlan softmac service: %s\n", zx_status_get_string(status));
      completer.buffer(arena).ReplyError(status);
      return;
    }
    served.Signal();
  });
  served.Wait();
  std::array<const char*, 1> offers{
      fuchsia_wlan_softmac::Service::Name,
  };
  // The outgoing directory will only be accessible by the driver that binds to
  // the newly created device.
  status = wlan_softmac_device->DdkAdd(::ddk::DeviceAddArgs("iwlwifi-wlan-softmac")
                                           .set_proto_id(ZX_PROTOCOL_WLAN_SOFTMAC)
                                           .set_runtime_service_offers(offers)
                                           .set_outgoing_dir(endpoints->client.TakeChannel()));
  if (status != ZX_OK) {
    IWL_ERR(this, "%s() failed mac device add: %s\n", __func__, zx_status_get_string(status));
    phy_create_iface_undo(drvdata(), out_iface_id);
    completer.buffer(arena).ReplyError(status);
    return;
  }
  wlan_softmac_device.release();

  IWL_INFO("%s() created iface %u\n", __func__, out_iface_id);

  fidl::Arena fidl_arena;
  auto builder = phyimpl_fidl::WlanPhyImplCreateIfaceResponse::Builder(fidl_arena);
  builder.iface_id(out_iface_id);
  completer.buffer(arena).ReplySuccess(builder.Build());
}

void WlanPhyImplDevice::DestroyIface(DestroyIfaceRequestView request, fdf::Arena& arena,
                                     DestroyIfaceCompleter::Sync& completer) {
  if (!request->has_iface_id()) {
    IWL_ERR(this, "%s() invoked without valid iface id\n", __func__);
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  IWL_INFO("%s() for iface %u\n", __func__, request->iface_id());
  zx_status_t status = phy_destroy_iface(drvdata(), request->iface_id());
  if (status != ZX_OK) {
    completer.buffer(arena).ReplyError(status);
    IWL_ERR(this, "%s() failed destroy iface: %s\n", __func__, zx_status_get_string(status));
    return;
  }

  completer.buffer(arena).ReplySuccess();
}

void WlanPhyImplDevice::SetCountry(SetCountryRequestView request, fdf::Arena& arena,
                                   SetCountryCompleter::Sync& completer) {
  wlan_phy_country_t country;

  if (!request->is_alpha2()) {
    IWL_ERR(this, "%s() only alpha2 format is supported\n", __func__);
    completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  memcpy(&country.alpha2[0], request->alpha2().data(), WLANPHY_ALPHA2_LEN);
  zx_status_t status = phy_set_country(drvdata(), &country);
  if (status != ZX_OK) {
    IWL_ERR(this, "%s() failed set country: %s\n", __func__, zx_status_get_string(status));
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
    IWL_ERR(this, "%s() failed get country: %s\n", __func__, zx_status_get_string(status));
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

}  // namespace wlan::iwlwifi
