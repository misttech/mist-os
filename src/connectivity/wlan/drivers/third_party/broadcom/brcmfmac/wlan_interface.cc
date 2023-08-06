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

#include "fuchsia/hardware/wlan/fullmac/c/banjo.h"
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
    : ddk::Device<WlanInterface, ddk::Unbindable>(device->parent()),
      NetworkPort(proto, *this, port_id),
      wdev_(nullptr),
      device_(nullptr),
      outgoing_dir_(fdf::OutgoingDirectory::Create(fdf::Dispatcher::GetCurrent()->get())) {
  zx_status_t status = ZX_OK;

  // The protocol to let pass device_add. This is to support FakeDevMgr device lifecycle management
  // in SIM tests, we can get rid of it after everything get into DFv2 world including driver
  // unittest framework.
  static const zx_protocol_device_t device_proto = {
      .version = DEVICE_OPS_VERSION,
      .release = [](void* ctx) { return static_cast<WlanInterface*>(ctx)->DdkRelease(); },
  };

  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    BRCMF_ERR("failed to create endpoints: %s\n", endpoints.status_string());
    return;
  }

  status = ServeWlanFullmacImplProtocol(std::move(endpoints->server));
  if (status != ZX_OK) {
    BRCMF_ERR("failed to serve WlanFullmacImpl service: %s\n", zx_status_get_string(status));
    return;
  }

  std::array offers = {
      fuchsia_wlan_fullmac::Service::Name,
  };

  device_add_args device_args = {
      .version = DEVICE_ADD_ARGS_VERSION,
      .name = name,
      .ctx = this,
      .ops = &device_proto,
      .proto_id = ZX_PROTOCOL_WLAN_FULLMAC_IMPL,
      .runtime_service_offers = offers.data(),
      .runtime_service_offer_count = offers.size(),
      .outgoing_dir_channel = endpoints->client.TakeChannel().release(),
  };

  if ((status = device->DeviceAdd(&device_args, &this->zxdev_)) != ZX_OK) {
    BRCMF_ERR("failed to add interface device: %s\n", zx_status_get_string(status));
    return;
  }
}

zx_status_t WlanInterface::Create(wlan::brcmfmac::Device* device, const char* name,
                                  wireless_dev* wdev, wlan_mac_role_t role,
                                  WlanInterface** out_interface) {
  std::unique_ptr<WlanInterface> interface(new WlanInterface(
      device, device->NetDev().NetDevIfcProto(), ndev_to_if(wdev->netdev)->ifidx, name));
  {
    std::lock_guard<std::shared_mutex> guard(interface->lock_);
    interface->device_ = device;
    interface->wdev_ = wdev;
  }

  NetworkPort::Role net_port_role;
  switch (role) {
    case WLAN_MAC_ROLE_CLIENT:
      net_port_role = NetworkPort::Role::Client;
      break;
    case WLAN_MAC_ROLE_AP:
      net_port_role = NetworkPort::Role::Ap;
      break;
    default:
      BRCMF_ERR("Unsupported role %u", role);
      return ZX_ERR_INVALID_ARGS;
  }

  interface->NetworkPort::Init(net_port_role);

  *out_interface = interface.release();  // This now has its lifecycle managed by the devhost.
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
  // Just like DeviceAdd(), we also have to call into the phy device for invoking different handlers
  // from different buses.
  device_->DeviceAsyncRemove(this->zxdev_);
}

void WlanInterface::DdkUnbind(ddk::UnbindTxn txn) {
  zx::result res =
      outgoing_dir_.RemoveService<fuchsia_wlan_fullmac::Service>(fdf::kDefaultInstance);
  if (res.is_error()) {
    BRCMF_ERR("Failed to remove WlanFullmacImpl service from outgoing directory: %s\n",
              res.status_string());
  }
  txn.Reply();
}

void WlanInterface::DdkRelease() {
  fit::callback<void()> on_remove;
  {
    std::lock_guard lock(lock_);
    if (on_remove_) {
      on_remove = std::move(on_remove_);
    }
  }
  delete this;
  if (on_remove) {
    on_remove();
  }
}

zx_status_t WlanInterface::ServeWlanFullmacImplProtocol(
    fidl::ServerEnd<fuchsia_io::Directory> server_end) {
  auto protocol = [this](fdf::ServerEnd<fuchsia_wlan_fullmac::WlanFullmacImpl> server_end) mutable {
    fdf::BindServer(fdf::Dispatcher::GetCurrent()->get(), std::move(server_end), this);
  };
  fuchsia_wlan_fullmac::Service::InstanceHandler handler(
      {.wlan_fullmac_impl = std::move(protocol)});
  auto status = outgoing_dir_.AddService<fuchsia_wlan_fullmac::Service>(std::move(handler));
  if (status.is_error()) {
    BRCMF_ERR("Failed to add service to outgoing directory: %s\n", status.status_string());
    return status.error_value();
  }
  auto result = outgoing_dir_.Serve(std::move(server_end));
  if (result.is_error()) {
    BRCMF_ERR("Failed to serve outgoing directory: %s\n", result.status_string());
    return result.error_value();
  }

  return ZX_OK;
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

zx_status_t WlanInterface::SetCountry(brcmf_pub* drvr, const wlan_phy_country_t* country) {
  if (country == nullptr) {
    BRCMF_ERR("Empty country from the parameter.");
    return ZX_ERR_INVALID_ARGS;
  }
  return brcmf_set_country(drvr, country);
}

zx_status_t WlanInterface::GetCountry(brcmf_pub* drvr, wlan_phy_country_t* out_country) {
  return brcmf_get_country(drvr, out_country);
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
    wdev_->netdev->if_proto = std::make_unique<WlanFullmacIfc>(std::move(request->ifc));
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

void WlanInterface::ConnectReq(ConnectReqRequestView request, fdf::Arena& arena,
                               ConnectReqCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  if (wdev_ != nullptr) {
    brcmf_if_connect_req(wdev_->netdev, request);
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::ReconnectReq(ReconnectReqRequestView request, fdf::Arena& arena,
                                 ReconnectReqCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  const fuchsia_wlan_fullmac::wire::WlanFullmacReconnectReq req = request->req;
  if (wdev_ != nullptr) {
    brcmf_if_reconnect_req(wdev_->netdev, &req);
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::AuthResp(AuthRespRequestView request, fdf::Arena& arena,
                             AuthRespCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  const fuchsia_wlan_fullmac::wire::WlanFullmacAuthResp resp = request->resp;
  if (wdev_ != nullptr) {
    brcmf_if_auth_resp(wdev_->netdev, &resp);
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::DeauthReq(DeauthReqRequestView request, fdf::Arena& arena,
                              DeauthReqCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  const fuchsia_wlan_fullmac::wire::WlanFullmacDeauthReq req = request->req;
  if (wdev_ != nullptr) {
    brcmf_if_deauth_req(wdev_->netdev, &req);
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::AssocResp(AssocRespRequestView request, fdf::Arena& arena,
                              AssocRespCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  const fuchsia_wlan_fullmac::wire::WlanFullmacAssocResp resp = request->resp;
  if (wdev_ != nullptr) {
    brcmf_if_assoc_resp(wdev_->netdev, &resp);
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::DisassocReq(DisassocReqRequestView request, fdf::Arena& arena,
                                DisassocReqCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  const fuchsia_wlan_fullmac::wire::WlanFullmacDisassocReq req = request->req;
  if (wdev_ != nullptr) {
    brcmf_if_disassoc_req(wdev_->netdev, &req);
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::ResetReq(ResetReqRequestView request, fdf::Arena& arena,
                             ResetReqCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  const fuchsia_wlan_fullmac::wire::WlanFullmacResetReq req = request->req;
  if (wdev_ != nullptr) {
    brcmf_if_reset_req(wdev_->netdev, &req);
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::StartReq(StartReqRequestView request, fdf::Arena& arena,
                             StartReqCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  const fuchsia_wlan_fullmac::wire::WlanFullmacStartReq req = request->req;
  if (wdev_ != nullptr) {
    brcmf_if_start_req(wdev_->netdev, &req);
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::StopReq(StopReqRequestView request, fdf::Arena& arena,
                            StopReqCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  const fuchsia_wlan_fullmac::wire::WlanFullmacStopReq req = request->req;
  if (wdev_ != nullptr) {
    brcmf_if_stop_req(wdev_->netdev, &req);
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

void WlanInterface::EapolReq(EapolReqRequestView request, fdf::Arena& arena,
                             EapolReqCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  const fuchsia_wlan_fullmac::wire::WlanFullmacEapolReq req = request->req;
  if (wdev_ != nullptr) {
    brcmf_if_eapol_req(wdev_->netdev, &req);
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

void WlanInterface::StartCaptureFrames(StartCaptureFramesRequestView request, fdf::Arena& arena,
                                       StartCaptureFramesCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  const fuchsia_wlan_fullmac::wire::WlanFullmacStartCaptureFramesReq req = request->req;
  fuchsia_wlan_fullmac::wire::WlanFullmacStartCaptureFramesResp resp;
  if (wdev_ != nullptr) {
    brcmf_if_start_capture_frames(wdev_->netdev, &req, &resp);
  }
  completer.buffer(arena).Reply(resp);
}

void WlanInterface::StopCaptureFrames(fdf::Arena& arena,
                                      StopCaptureFramesCompleter::Sync& completer) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  if (wdev_ != nullptr) {
    brcmf_if_stop_capture_frames(wdev_->netdev);
  }
  completer.buffer(arena).Reply();
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

void WlanInterface::MacGetAddress(uint8_t out_mac[MAC_SIZE]) {
  std::shared_lock<std::shared_mutex> guard(lock_);
  if (wdev_ == nullptr) {
    BRCMF_WARN("Interface not available, returning empty MAC address");
    memset(out_mac, 0, MAC_SIZE);
    return;
  }
  memcpy(out_mac, ndev_to_if(wdev_->netdev)->mac_addr, MAC_SIZE);
}

void WlanInterface::MacGetFeatures(features_t* out_features) {
  *out_features = {
      .multicast_filter_count = 0,
      .supported_modes = SUPPORTED_MAC_FILTER_MODE_MULTICAST_FILTER |
                         SUPPORTED_MAC_FILTER_MODE_MULTICAST_PROMISCUOUS,
  };
}

void WlanInterface::MacSetMode(mode_t mode, cpp20::span<const uint8_t> multicast_macs) {
  zx_status_t status = ZX_OK;
  std::shared_lock<std::shared_mutex> guard(lock_);
  switch (mode) {
    case MODE_MULTICAST_FILTER:
      status = brcmf_if_set_multicast_promisc(wdev_->netdev, false);
      break;
    case MODE_MULTICAST_PROMISCUOUS:
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
