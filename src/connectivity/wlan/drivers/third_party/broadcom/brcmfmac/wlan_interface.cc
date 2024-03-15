// Copyright (c) 2019 The Fuchsia Authors
//
// Permission to use, copy, modify, and/or distribute this software for any purpose with or without
// fee is hereby granted, provided that the above copyright notice and this permission notice
// appear in all copies.
//
// THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH REGARD TO THIS
// SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE
// AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
// WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT,
// NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE
// OF THIS SOFTWARE.

#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/wlan_interface.h"

#include <fuchsia/hardware/network/driver/c/banjo.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include <cstdio>
#include <cstring>

#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/cfg80211.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/common.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/debug.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/device.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/feature.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/fwil.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/linuxisms.h"

namespace wlan {
namespace brcmfmac {
namespace {

constexpr uint32_t kEthernetMtu = 1500;

}  // namespace

WlanInterface::WlanInterface(wlan::brcmfmac::Device* device,
                             const network_device_ifc_protocol_t& proto, uint8_t port_id,
                             const char* name)
    : NetworkPort(proto, *this, port_id), wdev_(nullptr), device_(nullptr), name_(name) {}

zx::result<std::unique_ptr<WlanInterface>> WlanInterface::Create(
    wlan::brcmfmac::Device* device, const char* name, wireless_dev* wdev,
    fuchsia_wlan_common_wire::WlanMacRole role) {
  std::unique_ptr<WlanInterface> interface(new WlanInterface(
      device, device->NetDev()->NetDevIfcProto(), ndev_to_if(wdev->netdev)->ifidx, name));
  {
    std::lock_guard<std::shared_mutex> guard(interface->lock_);
    interface->device_ = device;
    interface->wdev_ = wdev;
  }

  interface->role_ = role;
  zx_status_t status;

  if ((status = interface->AddWlanFullmacDevice()) != ZX_OK) {
    BRCMF_ERR("Error while adding fullmac dev: %s", zx_status_get_string(status));
    return zx::error(status);
  }
  NetworkPort::Role net_port_role;
  switch (role) {
    case fuchsia_wlan_common_wire::WlanMacRole::kClient:
      net_port_role = NetworkPort::Role::Client;
      break;
    case fuchsia_wlan_common_wire::WlanMacRole::kAp:
      net_port_role = NetworkPort::Role::Ap;
      break;
    default:
      BRCMF_ERR("Unsupported role %u", uint32_t(role));
      return zx::error(ZX_ERR_INVALID_ARGS);
  }

  status = interface->NetworkPort::Init(net_port_role);
  if (status != ZX_OK) {
    BRCMF_ERR("Failed to initialize port: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  return zx::ok(std::move(interface));
}

zx_status_t WlanInterface::AddWlanFullmacDevice() {
  fdf_dispatcher_t* driver_dispatcher = fdf::Dispatcher::GetCurrent()->get();
  auto wlanfullmacimpl =
      [this, driver_dispatcher](fdf::ServerEnd<fuchsia_wlan_fullmac::WlanFullmacImpl> server_end) {
        ServiceConnectHandler(driver_dispatcher, std::move(server_end));
      };

  // Add the service contains WlanFullmac protocol to outgoing directory.
  fuchsia_wlan_fullmac::Service::InstanceHandler wlanfullmac_service_handler(
      {.wlan_fullmac_impl = wlanfullmacimpl});

  auto status = device_->Outgoing()->AddService<fuchsia_wlan_fullmac::Service>(
      std::move(wlanfullmac_service_handler), GetName());
  if (status.is_error()) {
    BRCMF_ERR("Failed to add service to outgoing directory: %s", status.status_string());
    return status.status_value();
  }

  fidl::Arena arena;

  fidl::VectorView<fuchsia_driver_framework::wire::Offer> offers(arena, 1);
  offers[0] = fdf::MakeOffer2<fuchsia_wlan_fullmac::Service>(arena, GetName());
  auto property = fdf::MakeProperty(arena, BIND_PROTOCOL, ZX_PROTOCOL_WLAN_FULLMAC_IMPL);

  auto args = fdf::wire::NodeAddArgs::Builder(arena)
                  .name(arena, GetName())
                  .properties(fidl::VectorView<fdf::wire::NodeProperty>::FromExternal(&property, 1))
                  .offers2(offers)
                  .Build();

  auto endpoints = fidl::CreateEndpoints<fdf::NodeController>();
  if (endpoints.is_error()) {
    return endpoints.status_value();
  }

  wlanfullmac_controller_.Bind(std::move(endpoints->client),
                               fdf::Dispatcher::GetCurrent()->async_dispatcher(), this);
  // Add wlanfullmac child node for the node that this driver is binding to. Doing a sync version
  // here to reduce chaos.
  auto result =
      device_->GetParentNode().sync()->AddChild(std::move(args), std::move(endpoints->server), {});
  if (!result.ok()) {
    BRCMF_ERR("Add wlanfullmac node error due to FIDL error on protocol [Node]: %s",
              result.status_string());
    return result.status();
  }
  if (result->is_error()) {
    BRCMF_ERR("Add wlanfullmac node error: %u", static_cast<uint32_t>(result->error_value()));
    return ZX_ERR_INTERNAL;
  }
  return ZX_OK;
}

zx_status_t WlanInterface::RemoveWlanFullmacDevice() {
  if (!wlanfullmac_controller_.is_valid()) {
    BRCMF_ERR("Fullmac device for role %u cannot be removed because controller is invalid", Role());
    return ZX_ERR_BAD_STATE;
  }
  auto result = wlanfullmac_controller_->Remove();
  if (!result.ok()) {
    BRCMF_ERR("Fullmac child remove failed for role %u, FIDL error: %s", Role(),
              result.status_string());
    return result.status();
  }
  wlanfullmac_controller_ = {};

  auto remove_result = device_->Outgoing()->RemoveService<fuchsia_wlan_fullmac::Service>(GetName());
  if (remove_result.is_error()) {
    BRCMF_ERR("Failed to remove wlanfullmac service from outgoing directory: %s.",
              remove_result.status_string());
    return remove_result.status_value();
  }
  return ZX_OK;
}

zx_status_t WlanInterface::DestroyIface() {
  zx_status_t status = RemoveWlanFullmacDevice();
  if (status != ZX_OK) {
    BRCMF_ERR("Device::RemoveWlanFullmacDevice() failed for Client interface : %s",
              zx_status_get_string(status));

    // If ZX_ERR_BAD_STATE is returned, we may have previously called RemoveWlanFullmacDevice
    // successfully but failed to delete the iface from firmware.
    // In that case, we don't return here to try deleting the iface from firmware again to avoid
    // having an iface in firmware that we can never delete.
    if (status != ZX_ERR_BAD_STATE) {
      return status;
    }
  }

  RemovePort();
  wireless_dev* wdev = take_wdev();

  if ((status = brcmf_cfg80211_del_iface(device_->drvr()->config, wdev)) != ZX_OK) {
    BRCMF_ERR("Failed to del iface, status: %s", zx_status_get_string(status));
    set_wdev(wdev);
    return status;
  }
  return ZX_OK;
}

void WlanInterface::set_wdev(wireless_dev* wdev) {
  std::lock_guard<std::shared_mutex> guard(lock_);
  wdev_ = wdev;
}

wireless_dev* WlanInterface::take_wdev() {
  std::lock_guard<std::shared_mutex> guard(lock_);
  wireless_dev* wdev = wdev_;
  wdev_ = nullptr;
  return wdev;
}

void WlanInterface::Remove(fit::callback<void()>&& on_remove) {
  {
    std::lock_guard lock(lock_);
    on_remove_ = std::move(on_remove);
  }
}

void WlanInterface::ServiceConnectHandler(
    fdf_dispatcher_t* dispatcher,
    fdf::ServerEnd<fuchsia_wlan_fullmac::WlanFullmacImpl> server_end) {
  bindings_.AddBinding(dispatcher, std::move(server_end), this, [](fidl::UnbindInfo info) {
    if (!info.is_user_initiated()) {
      BRCMF_ERR("WlanFullmacImpl binding unexpectedly closed: %s", info.lossy_description());
    }
  });
}

zx_status_t WlanInterface::GetSupportedMacRoles(
    struct brcmf_pub* drvr,
    fuchsia_wlan_common::wire::WlanMacRole
        out_supported_mac_roles_list[fuchsia_wlan_common::wire::kMaxSupportedMacRoles],
    uint8_t* out_supported_mac_roles_count) {
  // The default client iface at bsscfgidx 0 is always assumed to exist by the driver.
  if (!drvr->iflist[0]) {
    BRCMF_ERR("drvr->iflist[0] is NULL. This should never happen.");
    return ZX_ERR_INTERNAL;
  }

  size_t len = 0;
  if (brcmf_feat_is_enabled(drvr, BRCMF_FEAT_STA)) {
    out_supported_mac_roles_list[len] = fuchsia_wlan_common::wire::WlanMacRole::kClient;
    ++len;
  }
  if (brcmf_feat_is_enabled(drvr, BRCMF_FEAT_AP)) {
    out_supported_mac_roles_list[len] = fuchsia_wlan_common::wire::WlanMacRole::kAp;
    ++len;
  }
  *out_supported_mac_roles_count = len;

  return ZX_OK;
}

zx_status_t WlanInterface::SetCountry(brcmf_pub* drvr,
                                      const fuchsia_wlan_phyimpl_wire::WlanPhyCountry* country) {
  if (country == nullptr) {
    BRCMF_ERR("Empty country from the parameter.");
    return ZX_ERR_INVALID_ARGS;
  }
  return brcmf_set_country(drvr, country);
}

zx_status_t WlanInterface::GetCountry(brcmf_pub* drvr, uint8_t* cc_code) {
  return brcmf_get_country(drvr, cc_code);
}

zx_status_t WlanInterface::ClearCountry(brcmf_pub* drvr) { return brcmf_clear_country(drvr); }

void WlanInterface::Start(StartRequestView request, fdf::Arena& arena,
                          StartCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  if (wdev_ == nullptr) {
    BRCMF_ERR("Failed to start interface: wdev_ not found.");
    completer.buffer(arena).ReplyError(ZX_ERR_BAD_STATE);
    return;
  }

  {
    std::lock_guard<std::shared_mutex> guard(wdev_->netdev->if_proto_lock);
    wdev_->netdev->if_proto =
        fdf::WireSyncClient<fuchsia_wlan_fullmac::WlanFullmacImplIfc>(std::move(request->ifc));
  }

  zx::channel out_mlme_channel;
  zx_status_t status = brcmf_if_start(wdev_->netdev, (zx_handle_t*)&out_mlme_channel);
  if (status != ZX_OK) {
    BRCMF_ERR("Failed to start interface: %s", zx_status_get_string(status));
    completer.buffer(arena).ReplyError(status);
    return;
  }
  completer.buffer(arena).ReplySuccess(std::move(out_mlme_channel));
}

void WlanInterface::Stop(fdf::Arena& arena, StopCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  if (wdev_ != nullptr) {
    brcmf_if_stop(wdev_->netdev);
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::Query(fdf::Arena& arena, QueryCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  fuchsia_wlan_fullmac::wire::WlanFullmacQueryInfo info;
  if (wdev_ != nullptr) {
    brcmf_if_query(wdev_->netdev, &info);
  }
  completer.buffer(arena).ReplySuccess(info);
}

void WlanInterface::QueryMacSublayerSupport(fdf::Arena& arena,
                                            QueryMacSublayerSupportCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  fuchsia_wlan_common::wire::MacSublayerSupport resp;
  if (wdev_ != nullptr) {
    brcmf_if_query_mac_sublayer_support(wdev_->netdev, &resp);
  }
  completer.buffer(arena).ReplySuccess(resp);
}

void WlanInterface::QuerySecuritySupport(fdf::Arena& arena,
                                         QuerySecuritySupportCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  fuchsia_wlan_common::wire::SecuritySupport resp;
  if (wdev_ != nullptr) {
    brcmf_if_query_security_support(wdev_->netdev, &resp);
  }
  completer.buffer(arena).ReplySuccess(resp);
}

void WlanInterface::QuerySpectrumManagementSupport(
    fdf::Arena& arena, QuerySpectrumManagementSupportCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  fuchsia_wlan_common::wire::SpectrumManagementSupport resp;
  if (wdev_ != nullptr) {
    brcmf_if_query_spectrum_management_support(wdev_->netdev, &resp);
  }
  completer.buffer(arena).ReplySuccess(resp);
}

void WlanInterface::StartScan(StartScanRequestView request, fdf::Arena& arena,
                              StartScanCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  if (wdev_ != nullptr) {
    brcmf_if_start_scan(wdev_->netdev, request);
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::Connect(ConnectRequestView request, fdf::Arena& arena,
                            ConnectCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  if (wdev_ != nullptr) {
    brcmf_if_connect_req(wdev_->netdev, request);
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::Reconnect(ReconnectRequestView request, fdf::Arena& arena,
                              ReconnectCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  if (wdev_ != nullptr) {
    brcmf_if_reconnect_req(wdev_->netdev, request);
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::AuthResp(AuthRespRequestView request, fdf::Arena& arena,
                             AuthRespCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  if (wdev_ != nullptr) {
    brcmf_if_auth_resp(wdev_->netdev, request);
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::Deauth(DeauthRequestView request, fdf::Arena& arena,
                           DeauthCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  if (wdev_ != nullptr) {
    brcmf_if_deauth_req(wdev_->netdev, request);
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::AssocResp(AssocRespRequestView request, fdf::Arena& arena,
                              AssocRespCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  if (wdev_ != nullptr) {
    brcmf_if_assoc_resp(wdev_->netdev, request);
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::Disassoc(DisassocRequestView request, fdf::Arena& arena,
                             DisassocCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  if (wdev_ != nullptr) {
    brcmf_if_disassoc_req(wdev_->netdev, request);
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::Reset(ResetRequestView request, fdf::Arena& arena,
                          ResetCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  if (wdev_ != nullptr) {
    brcmf_if_reset_req(wdev_->netdev, request);
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::StartBss(StartBssRequestView request, fdf::Arena& arena,
                             StartBssCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  if (wdev_ != nullptr) {
    brcmf_if_start_req(wdev_->netdev, request);
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::StopBss(StopBssRequestView request, fdf::Arena& arena,
                            StopBssCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  if (wdev_ != nullptr) {
    brcmf_if_stop_req(wdev_->netdev, request);
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::SetKeysReq(SetKeysReqRequestView request, fdf::Arena& arena,
                               SetKeysReqCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  const fuchsia_wlan_fullmac::wire::WlanFullmacSetKeysReq req = request->req;
  fuchsia_wlan_fullmac::wire::WlanFullmacSetKeysResp resp;
  if (wdev_ != nullptr) {
    brcmf_if_set_keys_req(wdev_->netdev, &req, &resp);
  }

  completer.buffer(arena).Reply(resp);
}

void WlanInterface::DelKeysReq(DelKeysReqRequestView request, fdf::Arena& arena,
                               DelKeysReqCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  const fuchsia_wlan_fullmac::wire::WlanFullmacDelKeysReq req = request->req;
  if (wdev_ != nullptr) {
    brcmf_if_del_keys_req(wdev_->netdev, &req);
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::EapolTx(EapolTxRequestView request, fdf::Arena& arena,
                            EapolTxCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  if (wdev_ != nullptr) {
    brcmf_if_eapol_req(wdev_->netdev, request);
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::GetIfaceCounterStats(fdf::Arena& arena,
                                         GetIfaceCounterStatsCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  fuchsia_wlan_fullmac::wire::WlanFullmacIfaceCounterStats out_stats;
  if (wdev_ == nullptr) {
    completer.buffer(arena).ReplyError(ZX_ERR_BAD_STATE);
    return;
  }
  zx_status_t status = brcmf_if_get_iface_counter_stats(wdev_->netdev, &out_stats);
  if (status != ZX_OK) {
    completer.buffer(arena).ReplyError(status);
  } else {
    completer.buffer(arena).ReplySuccess(out_stats);
  }
}

// Max size of WlanFullmacIfaceHistogramStats.
constexpr size_t kWlanFullmacIfaceHistogramStatsBufferSize =
    fidl::MaxSizeInChannel<fuchsia_wlan_fullmac::wire::WlanFullmacIfaceHistogramStats,
                           fidl::MessageDirection::kSending>();

void WlanInterface::GetIfaceHistogramStats(fdf::Arena& arena,
                                           GetIfaceHistogramStatsCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  fidl::Arena<kWlanFullmacIfaceHistogramStatsBufferSize> table_arena;
  fuchsia_wlan_fullmac::wire::WlanFullmacIfaceHistogramStats out_stats;
  if (wdev_ == nullptr) {
    completer.buffer(arena).ReplyError(ZX_ERR_BAD_STATE);
    return;
  }
  zx_status_t status = brcmf_if_get_iface_histogram_stats(wdev_->netdev, &out_stats, table_arena);
  if (status != ZX_OK) {
    completer.buffer(arena).ReplyError(status);
  } else {
    completer.buffer(arena).ReplySuccess(out_stats);
  }
}

void WlanInterface::SetMulticastPromisc(SetMulticastPromiscRequestView request, fdf::Arena& arena,
                                        SetMulticastPromiscCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  if (wdev_ == nullptr) {
    completer.buffer(arena).ReplyError(ZX_ERR_BAD_STATE);
    return;
  }
  bool enable = request->enable;
  zx_status_t status = brcmf_if_set_multicast_promisc(wdev_->netdev, enable);
  if (status != ZX_OK) {
    completer.buffer(arena).ReplyError(status);
  } else {
    completer.buffer(arena).ReplySuccess();
  }
}

void WlanInterface::SaeHandshakeResp(SaeHandshakeRespRequestView request, fdf::Arena& arena,
                                     SaeHandshakeRespCompleter::Sync& completer) {
  const fuchsia_wlan_fullmac::wire::WlanFullmacSaeHandshakeResp resp = request->resp;
  brcmf_if_sae_handshake_resp(wdev_->netdev, &resp);
  completer.buffer(arena).Reply();
}

void WlanInterface::SaeFrameTx(SaeFrameTxRequestView request, fdf::Arena& arena,
                               SaeFrameTxCompleter::Sync& completer) {
  const fuchsia_wlan_fullmac::wire::WlanFullmacSaeFrame frame = request->frame;
  brcmf_if_sae_frame_tx(wdev_->netdev, &frame);
  completer.buffer(arena).Reply();
}

void WlanInterface::WmmStatusReq(fdf::Arena& arena, WmmStatusReqCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  if (wdev_ != nullptr) {
    brcmf_if_wmm_status_req(wdev_->netdev);
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::OnLinkStateChanged(OnLinkStateChangedRequestView request, fdf::Arena& arena,
                                       OnLinkStateChangedCompleter::Sync& completer) {
  {
    std::shared_lock<std::shared_mutex> guard(lock_);
    bool online = request->online;
    SetPortOnline(online);
  }
  completer.buffer(arena).Reply();
}

uint32_t WlanInterface::PortGetMtu() { return kEthernetMtu; }

void WlanInterface::MacGetAddress(mac_address_t* out_mac) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  if (wdev_ == nullptr) {
    BRCMF_WARN("Interface not available, returning empty MAC address");
    memset(out_mac, 0, MAC_SIZE);
    return;
  }
  memcpy(out_mac->octets, ndev_to_if(wdev_->netdev)->mac_addr, MAC_SIZE);
}

void WlanInterface::MacGetFeatures(features_t* out_features) {
  *out_features = {
      .multicast_filter_count = 0,
      .supported_modes = SUPPORTED_MAC_FILTER_MODE_MULTICAST_FILTER |
                         SUPPORTED_MAC_FILTER_MODE_MULTICAST_PROMISCUOUS,
  };
}

void WlanInterface::MacSetMode(mode_t mode, cpp20::span<const mac_address_t> multicast_macs) {
  zx_status_t status = ZX_OK;
  std::shared_lock<std::shared_mutex> guard(lock_);
  switch (mode) {
    case MAC_FILTER_MODE_MULTICAST_FILTER:
      status = brcmf_if_set_multicast_promisc(wdev_->netdev, false);
      break;
    case MAC_FILTER_MODE_MULTICAST_PROMISCUOUS:
      status = brcmf_if_set_multicast_promisc(wdev_->netdev, true);
      break;
    default:
      BRCMF_ERR("Unsupported MAC mode: %u", mode);
      break;
  }

  if (status != ZX_OK) {
    BRCMF_ERR("MacSetMode failed: %s", zx_status_get_string(status));
  }
}

}  // namespace brcmfmac
}  // namespace wlan
