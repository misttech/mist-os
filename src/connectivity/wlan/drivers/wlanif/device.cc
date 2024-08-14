// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "device.h"

#include <fuchsia/wlan/fullmac/c/banjo.h>
#include <fuchsia/wlan/ieee80211/cpp/fidl.h>
#include <fuchsia/wlan/internal/c/banjo.h>
#include <fuchsia/wlan/internal/cpp/fidl.h>
#include <fuchsia/wlan/mlme/cpp/fidl.h>
#include <fuchsia/wlan/stats/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/device.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <net/ethernet.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include <memory>

#include <wlan/common/ieee80211_codes.h>

#include "convert.h"
#include "debug.h"
#include "fidl/fuchsia.wlan.fullmac/cpp/wire_types.h"
#include "fuchsia/wlan/common/c/banjo.h"
#include "fuchsia/wlan/common/cpp/fidl.h"
#include "zircon/system/public/zircon/assert.h"

namespace wlanif {

namespace wlan_common = ::fuchsia::wlan::common;
namespace wlan_ieee80211 = ::fuchsia::wlan::ieee80211;
namespace wlan_mlme = ::fuchsia::wlan::mlme;
namespace wlan_stats = ::fuchsia::wlan::stats;

Device::Device(fdf::DriverStartArgs start_args,
               fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : DriverBase("wlanif", std::move(start_args), std::move(driver_dispatcher)),
      parent_node_(fidl::WireClient(std::move(node()), dispatcher())) {
  zx::result logger = fdf::Logger::Create(*incoming(), dispatcher(), "wlanif", FUCHSIA_LOG_INFO);
  ZX_ASSERT_MSG(logger.is_ok(), "%s", logger.status_string());
  logger_ = std::move(logger.value());
  ltrace_fn(*logger_);
  // Establish the connection among histogram data lists, assuming each histogram list only contains
  // one histogram for now, will assert it in GetIfaceHistogramStats();
  noise_floor_histograms_.noise_floor_samples_list = noise_floor_buckets_;
  rssi_histograms_.rssi_samples_list = rssi_buckets_;
  rx_rate_index_histograms_.rx_rate_index_samples_list = rx_rate_index_buckets_;
  snr_histograms_.snr_samples_list = snr_buckets_;
}

Device::~Device() { ltrace_fn(*logger_); }

zx_status_t Device::InitMlme() {
  mlme_ = std::make_unique<FullmacMlme>(this);
  ZX_DEBUG_ASSERT(mlme_ != nullptr);
  zx_status_t status = mlme_->Init();
  if (status != ZX_OK) {
    mlme_.reset();
  }
  return status;
}

zx::result<> Device::Start() {
  {
    // Create a dispatcher to wait on the runtime channel
    auto dispatcher =
        fdf::SynchronizedDispatcher::Create(fdf::SynchronizedDispatcher::Options::kAllowSyncCalls,
                                            "wlan_fullmac_ifc_server", [&](fdf_dispatcher_t*) {});

    if (dispatcher.is_error()) {
      FDF_LOGL(ERROR, *logger_, "Creating server dispatcher error : %s",
               zx_status_get_string(dispatcher.status_value()));
      return zx::error(dispatcher.status_value());
    }
    server_dispatcher_ = *std::move(dispatcher);
  }
  {
    // Create a dispatcher to wait on the runtime channel
    auto dispatcher =
        fdf::SynchronizedDispatcher::Create(fdf::SynchronizedDispatcher::Options::kAllowSyncCalls,
                                            "wlan_fullmac_ifc_client", [&](fdf_dispatcher_t*) {});

    if (dispatcher.is_error()) {
      FDF_LOGL(ERROR, *logger_, "Creating client dispatcher error : %s",
               zx_status_get_string(dispatcher.status_value()));
      return zx::error(dispatcher.status_value());
    }
    client_dispatcher_ = *std::move(dispatcher);
  }
  auto endpoints = fidl::CreateEndpoints<fuchsia_wlan_fullmac::WlanFullmacImpl>();
  if (endpoints.is_error()) {
    FDF_LOGL(ERROR, *logger_, "Creating end point error: %s",
             zx_status_get_string(endpoints.status_value()));
    return zx::error(endpoints.status_value());
  }

  zx_status_t status;
  if ((status = ConnectToWlanFullmacImpl()) != ZX_OK) {
    FDF_LOGL(ERROR, *logger_, "Failed connecting to wlan fullmac impl driver: %s",
             zx_status_get_string(status));
    return zx::error(status);
  }

  if ((status = Bind()) != ZX_OK) {
    FDF_LOGL(ERROR, *logger_, "Failed adding wlan fullmac device: %s",
             zx_status_get_string(status));
    return zx::error(status);
  }

  return zx::ok();
}

void Device::PrepareStop(fdf::PrepareStopCompleter completer) {
  client_.AsyncTeardown();

  // Destroy the WlanFullmacImplIfc FFI bridge.
  {
    std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
    wlan_fullmac_impl_ifc_banjo_protocol_.reset();
  }
  if (mlme_.get()) {
    mlme_->StopMainLoop();
  }
  completer(zx::ok());
}

zx_status_t Device::Bind() {
  ltrace_fn(*logger_);
  auto mac_sublayer_arena = fdf::Arena::Create(0, 0);
  if (mac_sublayer_arena.is_error()) {
    FDF_LOGL(ERROR, *logger_, "Mac Sublayer arena creation failed: %s",
             mac_sublayer_arena.status_string());
    return ZX_ERR_INTERNAL;
  }

  auto mac_sublayer_result = client_.sync()->QueryMacSublayerSupport();

  if (!mac_sublayer_result.ok()) {
    FDF_LOGL(ERROR, *logger_, "QueryMacSublayerSupport failed  FIDL error: %s",
             mac_sublayer_result.status_string());
    return mac_sublayer_result.status();
  }
  if (mac_sublayer_result->is_error()) {
    FDF_LOGL(ERROR, *logger_, "QueryMacSublayerSupport failed : %s",
             zx_status_get_string(mac_sublayer_result->error_value()));
    return mac_sublayer_result->error_value();
  }

  if (mac_sublayer_result->value()->resp.data_plane.data_plane_type ==
      fuchsia_wlan_common::wire::DataPlaneType::kEthernetDevice) {
    FDF_LOGL(ERROR, *logger_, "Ethernet data plane is no longer supported by wlanif");
    return ZX_ERR_NOT_SUPPORTED;
  }

  return InitMlme();
}

zx_status_t Device::ConnectToWlanFullmacImpl() {
  zx::result<fidl::ClientEnd<fuchsia_wlan_fullmac::WlanFullmacImpl>> client_end =
      incoming()->Connect<fuchsia_wlan_fullmac::Service::WlanFullmacImpl>();
  if (client_end.is_error()) {
    FDF_LOGL(ERROR, *logger_, "Connect to FullmacImpl failed: %s", client_end.status_string());
    return client_end.status_value();
  }

  client_ =
      fidl::WireSharedClient(std::move(client_end.value()), client_dispatcher_.async_dispatcher());
  if (!client_.is_valid()) {
    FDF_LOGL(ERROR, *logger_, "WlanFullmacImpl Client is not valid");
    return ZX_ERR_BAD_HANDLE;
  }
  return ZX_OK;
}

zx_status_t Device::StartFullmac(const rust_wlan_fullmac_ifc_protocol_copy_t* ifc,
                                 zx::channel* out_sme_channel) {
  // We manually populate the protocol ops here so that we can verify at compile time
  // that our rust bindings have the expected parameters.
  wlan_fullmac_impl_ifc_banjo_protocol_ops_.reset(new wlan_fullmac_impl_ifc_banjo_protocol_ops_t{
      .on_scan_result = ifc->ops->on_scan_result,
      .on_scan_end = ifc->ops->on_scan_end,
      .connect_conf = ifc->ops->connect_conf,
      .roam_conf = ifc->ops->roam_conf,
      .auth_ind = ifc->ops->auth_ind,
      .deauth_conf = ifc->ops->deauth_conf,
      .deauth_ind = ifc->ops->deauth_ind,
      .assoc_ind = ifc->ops->assoc_ind,
      .disassoc_conf = ifc->ops->disassoc_conf,
      .disassoc_ind = ifc->ops->disassoc_ind,
      .start_conf = ifc->ops->start_conf,
      .stop_conf = ifc->ops->stop_conf,
      .eapol_conf = ifc->ops->eapol_conf,
      .on_channel_switch = ifc->ops->on_channel_switch,
      .signal_report = ifc->ops->signal_report,
      .eapol_ind = ifc->ops->eapol_ind,
      .on_pmk_available = ifc->ops->on_pmk_available,
      .sae_handshake_ind = ifc->ops->sae_handshake_ind,
      .sae_frame_rx = ifc->ops->sae_frame_rx,
      .on_wmm_status_resp = ifc->ops->on_wmm_status_resp,
  });

  {
    std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
    wlan_fullmac_impl_ifc_banjo_protocol_ =
        std::make_unique<wlan_fullmac_impl_ifc_banjo_protocol_t>();
    wlan_fullmac_impl_ifc_banjo_protocol_->ops = wlan_fullmac_impl_ifc_banjo_protocol_ops_.get();
    wlan_fullmac_impl_ifc_banjo_protocol_->ctx = ifc->ctx;
  }

  auto endpoints = fidl::CreateEndpoints<fuchsia_wlan_fullmac::WlanFullmacImplIfc>();
  if (endpoints.is_error()) {
    FDF_LOGL(ERROR, *logger_, "Creating endpoints error: %s",
             zx_status_get_string(endpoints.status_value()));
    return endpoints.status_value();
  }

  fidl::BindServer(server_dispatcher_.async_dispatcher(), std::move(endpoints->server), this,
                   std::mem_fn(&Device::OnUnbound));

  auto start_result = client_.sync()->Start(std::move(endpoints->client));

  if (!start_result.ok()) {
    FDF_LOGL(ERROR, *logger_, "Start failed, FIDL error: %s", start_result.status_string());
    return start_result.status();
  }

  if (start_result->is_error()) {
    FDF_LOGL(ERROR, *logger_, "Start failed: %s",
             zx_status_get_string(start_result->error_value()));
    return start_result->error_value();
  }

  *out_sme_channel = std::move(start_result->value()->sme_channel);
  return ZX_OK;
}

void Device::OnUnbound(fidl::UnbindInfo info,
                       fidl::ServerEnd<fuchsia_wlan_fullmac::WlanFullmacImplIfc>) {
  if (!info.is_user_initiated()) {
    FDF_LOGL(INFO, *logger_, "Wlanif shutting down: %s", info.FormatDescription().c_str());
  }
}

void Device::handle_unknown_event(
    fidl::UnknownEventMetadata<fuchsia_driver_framework::NodeController> metadata) {
  FDF_LOGL(ERROR, *logger_, "Received unknown NodeController FIDL event with ordinal %lu",
           metadata.event_ordinal);
}

void Device::StartScan(const wlan_fullmac_impl_start_scan_request_t* req) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    FDF_LOGL(ERROR, *logger_, "Arena creation failed: %s", arena.status_string());
    return;
  }
  fuchsia_wlan_fullmac::wire::WlanFullmacImplStartScanRequest scan_req;
  ConvertScanReq(*req, &scan_req, *arena);

  auto result = client_.sync()->StartScan(scan_req);

  if (!result.ok()) {
    FDF_LOGL(ERROR, *logger_, "StartScan failed, FIDL error: %s", result.status_string());
    return;
  }
}

void Device::Connect(const wlan_fullmac_impl_connect_request_t* req) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    FDF_LOGL(ERROR, *logger_, "Arena creation failed: %s", arena.status_string());
    return;
  }

  fuchsia_wlan_fullmac::wire::WlanFullmacImplConnectRequest connect_req;
  ConvertConnectReq(*req, &connect_req, *arena);

  auto result = client_.sync()->Connect(connect_req);

  if (!result.ok()) {
    FDF_LOGL(ERROR, *logger_, "ConnectReq failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::Reconnect(const wlan_fullmac_impl_reconnect_request_t* req) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    FDF_LOGL(ERROR, *logger_, "Arena creation failed: %s", arena.status_string());
    return;
  }
  auto builder = fuchsia_wlan_fullmac::wire::WlanFullmacImplReconnectRequest::Builder(*arena);
  // peer_sta_address
  ::fidl::Array<uint8_t, ETH_ALEN> peer_sta_address;
  std::memcpy(peer_sta_address.data(), req->peer_sta_address, ETH_ALEN);
  builder.peer_sta_address(peer_sta_address);

  auto result = client_.sync()->Reconnect(builder.Build());

  if (!result.ok()) {
    FDF_LOGL(ERROR, *logger_, "ReconnectReq failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::AuthenticateResp(const wlan_fullmac_impl_auth_resp_request_t* resp) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    FDF_LOGL(ERROR, *logger_, "Arena creation failed: %s", arena.status_string());
    return;
  }

  auto builder = fuchsia_wlan_fullmac::wire::WlanFullmacImplAuthRespRequest::Builder(*arena);
  // peer_sta_address
  ::fidl::Array<uint8_t, ETH_ALEN> peer_sta_address;
  std::memcpy(peer_sta_address.data(), resp->peer_sta_address, ETH_ALEN);
  builder.peer_sta_address(peer_sta_address);

  // result_code
  builder.result_code(ConvertAuthResult(resp->result_code));

  auto result = client_.sync()->AuthResp(builder.Build());

  if (!result.ok()) {
    FDF_LOGL(ERROR, *logger_, "AuthResp failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::Deauthenticate(const wlan_fullmac_impl_deauth_request_t* req) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    FDF_LOGL(ERROR, *logger_, "Arena creation failed: %s", arena.status_string());
    return;
  }
  auto builder = fuchsia_wlan_fullmac::wire::WlanFullmacImplDeauthRequest::Builder(*arena);

  ::fidl::Array<uint8_t, ETH_ALEN> peer_sta_address;
  std::memcpy(peer_sta_address.data(), req->peer_sta_address, ETH_ALEN);
  builder.peer_sta_address(peer_sta_address);
  builder.reason_code(fuchsia_wlan_ieee80211::ReasonCode(req->reason_code));

  auto result = client_.sync()->Deauth(builder.Build());

  if (!result.ok()) {
    FDF_LOGL(ERROR, *logger_, "DeauthReq failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::AssociateResp(const wlan_fullmac_impl_assoc_resp_request_t* resp) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    FDF_LOGL(ERROR, *logger_, "Arena creation failed: %s", arena.status_string());
    return;
  }
  auto builder = fuchsia_wlan_fullmac::wire::WlanFullmacImplAssocRespRequest::Builder(*arena);
  ::fidl::Array<uint8_t, ETH_ALEN> peer_sta_address;
  std::memcpy(peer_sta_address.data(), resp->peer_sta_address, ETH_ALEN);
  builder.peer_sta_address(peer_sta_address);
  builder.result_code(ConvertAssocResult(resp->result_code));
  builder.association_id(resp->association_id);

  auto result = client_.sync()->AssocResp(builder.Build());

  if (!result.ok()) {
    FDF_LOGL(ERROR, *logger_, "AssocResp failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::Disassociate(const wlan_fullmac_impl_disassoc_request_t* req) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    FDF_LOGL(ERROR, *logger_, "Arena creation failed: %s", arena.status_string());
    return;
  }

  auto builder = fuchsia_wlan_fullmac::wire::WlanFullmacImplDisassocRequest::Builder(*arena);
  ::fidl::Array<uint8_t, ETH_ALEN> peer_sta_address;
  std::memcpy(peer_sta_address.data(), req->peer_sta_address, ETH_ALEN);
  builder.peer_sta_address(peer_sta_address);
  builder.reason_code(static_cast<fuchsia_wlan_ieee80211::wire::ReasonCode>(req->reason_code));

  auto result = client_.sync()->Disassoc(builder.Build());

  if (!result.ok()) {
    FDF_LOGL(ERROR, *logger_, "DisassocReq failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::Reset(const wlan_fullmac_impl_reset_request_t* req) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    FDF_LOGL(ERROR, *logger_, "Arena creation failed: %s", arena.status_string());
    return;
  }
  auto builder = fuchsia_wlan_fullmac::wire::WlanFullmacImplResetRequest::Builder(*arena);

  ::fidl::Array<uint8_t, ETH_ALEN> sta_address;
  std::memcpy(sta_address.data(), req->sta_address, ETH_ALEN);
  builder.sta_address(sta_address);
  builder.set_default_mib(req->set_default_mib);

  auto result = client_.sync()->Reset(builder.Build());
  if (!result.ok()) {
    FDF_LOGL(ERROR, *logger_, "ResetReq failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::StartBss(const wlan_fullmac_impl_start_bss_request_t* req) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    FDF_LOGL(ERROR, *logger_, "Arena creation failed: %s", arena.status_string());
    return;
  }

  auto builder = fuchsia_wlan_fullmac::wire::WlanFullmacImplStartBssRequest::Builder(*arena);

  fuchsia_wlan_ieee80211::wire::CSsid ssid;
  ConvertCSsid(req->ssid, &ssid);
  builder.ssid(ssid);
  builder.bss_type(ConvertBssType(req->bss_type));
  builder.beacon_period(req->beacon_period);
  builder.dtim_period(req->dtim_period);
  builder.channel(req->channel);
  auto rsne = std::vector<uint8_t>(req->rsne_list, req->rsne_list + req->rsne_count);
  builder.rsne(fidl::VectorView<uint8_t>(*arena, rsne));
  auto vendor_ie =
      std::vector<uint8_t>(req->vendor_ie_list, req->vendor_ie_list + req->vendor_ie_count);
  builder.vendor_ie(fidl::VectorView<uint8_t>(*arena, vendor_ie));

  auto result = client_.sync()->StartBss(builder.Build());

  if (!result.ok()) {
    FDF_LOGL(ERROR, *logger_, "StartBss failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::StopBss(const wlan_fullmac_impl_stop_bss_request_t* req) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    FDF_LOGL(ERROR, *logger_, "Arena creation failed: %s", arena.status_string());
    return;
  }
  auto builder = fuchsia_wlan_fullmac::wire::WlanFullmacImplStopBssRequest::Builder(*arena);
  fuchsia_wlan_ieee80211::wire::CSsid ssid;
  ConvertCSsid(req->ssid, &ssid);
  builder.ssid(ssid);

  auto result = client_.sync()->StopBss(builder.Build());

  if (!result.ok()) {
    FDF_LOGL(ERROR, *logger_, "StopBss failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::SetKeysReq(const wlan_fullmac_set_keys_req_t* req,
                        wlan_fullmac_set_keys_resp_t* out_resp) {
  fuchsia_wlan_fullmac::wire::WlanFullmacSetKeysReq set_keys_req;

  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    FDF_LOGL(ERROR, *logger_, "Arena creation failed: %s", arena.status_string());
    return;
  }

  // Abort if too many keys sent.
  size_t num_keys = req->num_keys;
  if (num_keys > fuchsia_wlan_fullmac::wire::kWlanMaxKeylistSize) {
    FDF_LOGL(ERROR, *logger_, "Key list with length %zu exceeds maximum size %d", num_keys,
             WLAN_MAX_KEYLIST_SIZE);
    out_resp->num_keys = num_keys;
    for (size_t result_idx = 0; result_idx < num_keys; result_idx++) {
      out_resp->statuslist[result_idx] = ZX_ERR_INVALID_ARGS;
    }
    return;
  }

  set_keys_req.num_keys = num_keys;

  for (size_t desc_ndx = 0; desc_ndx < num_keys; desc_ndx++) {
    set_keys_req.keylist[desc_ndx] = ConvertWlanKeyConfig(req->keylist[desc_ndx], *arena);
  }

  auto result = client_.sync()->SetKeysReq(set_keys_req);

  if (!result.ok()) {
    FDF_LOGL(ERROR, *logger_, "SetKeyReq failed FIDL error: %s", result.status_string());
    return;
  }

  auto set_key_resp = result->resp;

  auto num_results = set_key_resp.num_keys;
  if (num_keys != num_results) {
    FDF_LOGL(ERROR, *logger_, "SetKeysReq count (%zu) and SetKeyResp count (%zu) do not agree",
             num_keys, num_results);
    return;
  }

  out_resp->num_keys = num_results;
  for (size_t result_idx = 0; result_idx < set_key_resp.num_keys; result_idx++) {
    out_resp->statuslist[result_idx] = set_key_resp.statuslist.data()[result_idx];
  }
}

void Device::DeleteKeysReq(const wlan_fullmac_del_keys_req_t* req) {
  fuchsia_wlan_fullmac::wire::WlanFullmacDelKeysReq delete_key_req;

  size_t num_keys = req->num_keys;
  if (num_keys > fuchsia_wlan_fullmac::wire::kWlanMaxKeylistSize) {
    FDF_LOG(WARNING, "truncating key list from %zu to %d members\n", num_keys,
            fuchsia_wlan_fullmac::wire::kWlanMaxKeylistSize);
    delete_key_req.num_keys = fuchsia_wlan_fullmac::wire::kWlanMaxKeylistSize;
  } else {
    delete_key_req.num_keys = num_keys;
  }
  for (size_t desc_ndx = 0; desc_ndx < delete_key_req.num_keys; desc_ndx++) {
    ConvertDeleteKeyDescriptor(req->keylist[desc_ndx], &delete_key_req.keylist[desc_ndx]);

    auto result = client_.sync()->DelKeysReq(delete_key_req);

    if (!result.ok()) {
      FDF_LOGL(ERROR, *logger_, "DelKeysReq failed FIDL error: %s", result.status_string());
      return;
    }
  }
}

void Device::EapolTx(const wlan_fullmac_impl_eapol_tx_request_t* req) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    FDF_LOGL(ERROR, *logger_, "Arena creation failed: %s", arena.status_string());
    return;
  }

  fidl::Array<uint8_t, ETH_ALEN> src_addr;
  fidl::Array<uint8_t, ETH_ALEN> dst_addr;
  std::memcpy(src_addr.data(), req->src_addr, ETH_ALEN);
  std::memcpy(dst_addr.data(), req->dst_addr, ETH_ALEN);

  auto data = fidl::VectorView(
      *arena, std::vector<uint8_t>(req->data_list, req->data_list + req->data_count));

  auto eapol_req = fuchsia_wlan_fullmac::wire::WlanFullmacImplEapolTxRequest::Builder(*arena)
                       .src_addr(src_addr)
                       .dst_addr(dst_addr)
                       .data(data)
                       .Build();

  auto result = client_.sync()->EapolTx(eapol_req);

  if (!result.ok()) {
    FDF_LOGL(ERROR, *logger_, "EapolTx failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::QueryDeviceInfo(wlan_fullmac_query_info_t* out_resp) {
  auto result = client_.sync()->Query();

  if (!result.ok()) {
    FDF_LOGL(ERROR, *logger_, "Query failed FIDL error: %s", result.status_string());
    return;
  }
  if (result->is_error()) {
    FDF_LOGL(ERROR, *logger_, "Query failed : %s", zx_status_get_string(result->error_value()));
    return;
  }

  auto& query_info = result->value()->info;
  ConvertQueryInfo(query_info, out_resp);
}

void Device::QueryMacSublayerSupport(mac_sublayer_support_t* out_resp) {
  auto result = client_.sync()->QueryMacSublayerSupport();

  if (!result.ok()) {
    FDF_LOGL(ERROR, *logger_, "Query failed FIDL error: %s", result.status_string());
    return;
  }
  if (result->is_error()) {
    FDF_LOGL(ERROR, *logger_, "Query failed : %s", zx_status_get_string(result->error_value()));
    return;
  }
  auto& mac_sublayer_support = result->value()->resp;
  ConvertMacSublayerSupport(mac_sublayer_support, out_resp);
}

void Device::QuerySecuritySupport(security_support_t* out_resp) {
  auto result = client_.sync()->QuerySecuritySupport();

  if (!result.ok()) {
    FDF_LOGL(ERROR, *logger_, "QuerySecuritySupport failed FIDL error: %s", result.status_string());
    return;
  }
  if (result->is_error()) {
    FDF_LOGL(ERROR, *logger_, "QuerySecuritySupport failed : %s",
             zx_status_get_string(result->error_value()));
    return;
  }

  auto& security_support = result->value()->resp;
  ConvertSecuritySupport(security_support, out_resp);
}

void Device::QuerySpectrumManagementSupport(spectrum_management_support_t* out_resp) {
  auto result = client_.sync()->QuerySpectrumManagementSupport();

  if (!result.ok()) {
    FDF_LOGL(ERROR, *logger_, "QuerySpectrumManagementSupport failed FIDL error: %s",
             result.status_string());
    return;
  }
  if (result->is_error()) {
    FDF_LOGL(ERROR, *logger_, "QuerySpectrumManagementSupport failed : %s",
             zx_status_get_string(result->error_value()));
    return;
  }

  auto& spectrum_management_support = result->value()->resp;
  ConvertSpectrumManagementSupport(spectrum_management_support, out_resp);
}

zx_status_t Device::GetIfaceCounterStats(wlan_fullmac_iface_counter_stats_t* out_stats) {
  auto result = client_.sync()->GetIfaceCounterStats();

  if (!result.ok()) {
    FDF_LOGL(ERROR, *logger_, "GetIfaceCounterStats failed FIDL error: %s", result.status_string());
    return result.status();
  }
  if (result->is_error()) {
    FDF_LOGL(ERROR, *logger_, "GetIfaceCounterStats failed : %s",
             zx_status_get_string(result->error_value()));
    return result->error_value();
  }
  auto& iface_counter_stats = result->value()->stats;
  ConvertIfaceCounterStats(iface_counter_stats, out_stats);
  return ZX_OK;
}

zx_status_t Device::GetIfaceHistogramStats(wlan_fullmac_iface_histogram_stats_t* out_stats) {
  std::lock_guard<std::mutex> lock(get_iface_histogram_stats_lock_);

  auto result = client_.sync()->GetIfaceHistogramStats();

  if (!result.ok()) {
    FDF_LOGL(ERROR, *logger_, "GetIfaceHistogramStats failed FIDL error: %s",
             result.status_string());
    return result.status();
  }
  if (result->is_error()) {
    FDF_LOGL(ERROR, *logger_, "GetIfaceHistogramStats failed : %s",
             zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  auto& iface_histogram_stats = result->value()->stats;

  // Faill hard if there are more than one histogram in each list.
  ZX_ASSERT(iface_histogram_stats.noise_floor_histograms().count() == 1);
  ZX_ASSERT(iface_histogram_stats.rssi_histograms().count() == 1);
  ZX_ASSERT(iface_histogram_stats.rx_rate_index_histograms().count() == 1);
  ZX_ASSERT(iface_histogram_stats.snr_histograms().count() == 1);

  ConvertNoiseFloorHistogram(iface_histogram_stats.noise_floor_histograms().data()[0],
                             &noise_floor_histograms_);
  ConvertRssiHistogram(iface_histogram_stats.rssi_histograms().data()[0], &rssi_histograms_);
  ConvertRxRateIndexHistogram(iface_histogram_stats.rx_rate_index_histograms().data()[0],
                              &rx_rate_index_histograms_);
  ConvertSnrHistogram(iface_histogram_stats.snr_histograms().data()[0], &snr_histograms_);

  out_stats->noise_floor_histograms_list = &noise_floor_histograms_;
  out_stats->rssi_histograms_list = &rssi_histograms_;
  out_stats->rx_rate_index_histograms_list = &rx_rate_index_histograms_;
  out_stats->snr_histograms_list = &snr_histograms_;
  out_stats->noise_floor_histograms_count = 1;
  out_stats->rssi_histograms_count = 1;
  out_stats->rx_rate_index_histograms_count = 1;
  out_stats->snr_histograms_count = 1;
  return ZX_OK;
}

void Device::OnLinkStateChanged(bool online) {
  auto result = client_.sync()->OnLinkStateChanged(online);

  if (!result.ok()) {
    FDF_LOGL(ERROR, *logger_, "OnLinkStateChanged failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::SaeHandshakeResp(const wlan_fullmac_sae_handshake_resp_t* resp) {
  fuchsia_wlan_fullmac::wire::WlanFullmacSaeHandshakeResp handshake_resp;

  ConvertSaeHandshakeResp(*resp, &handshake_resp);

  auto result = client_.sync()->SaeHandshakeResp(handshake_resp);

  if (!result.ok()) {
    FDF_LOGL(ERROR, *logger_, "SaeHandshakeResp failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::SaeFrameTx(const wlan_fullmac_sae_frame_t* frame) {
  fuchsia_wlan_fullmac::wire::WlanFullmacSaeFrame sae_frame;
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    FDF_LOGL(ERROR, *logger_, "Arena creation failed: %s", arena.status_string());
    return;
  }

  ConvertSaeFrame(*frame, &sae_frame, *arena);

  auto result = client_.sync()->SaeFrameTx(sae_frame);

  if (!result.ok()) {
    FDF_LOGL(ERROR, *logger_, "SaeFrameTx failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::WmmStatusReq() {
  auto result = client_.sync()->WmmStatusReq();

  if (!result.ok()) {
    FDF_LOGL(ERROR, *logger_, "WmmStatusReq failed FIDL error: %s", result.status_string());
    return;
  }
}

// Implementation of fuchsia_wlan_fullmac::WlanFullmacImplIfc.
void Device::OnScanResult(OnScanResultRequestView request, OnScanResultCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_scan_result_t scan_result;
    ConvertFullmacScanResult(request->result, &scan_result);
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->on_scan_result(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, &scan_result);
  }
  completer.Reply();
}

void Device::OnScanEnd(OnScanEndRequestView request, OnScanEndCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_scan_end_t scan_end;
    ConvertScanEnd(request->end, &scan_end);
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->on_scan_end(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, &scan_end);
  }
  completer.Reply();
}

void Device::ConnectConf(ConnectConfRequestView request, ConnectConfCompleter::Sync& completer) {
  wlan_fullmac_connect_confirm_t connect_conf;
  ConvertConnectConfirm(request->resp, &connect_conf);
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->connect_conf(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, &connect_conf);
  }
  completer.Reply();
}

void Device::RoamConf(RoamConfRequestView request, RoamConfCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_roam_confirm_t roam_conf;
    ConvertRoamConfirm(request->resp, &roam_conf);
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->roam_conf(wlan_fullmac_impl_ifc_banjo_protocol_->ctx,
                                                         &roam_conf);
  }
  completer.Reply();
}

void Device::AuthInd(AuthIndRequestView request, AuthIndCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_auth_ind_t auth_ind;
    ConvertAuthInd(request->resp, &auth_ind);
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->auth_ind(wlan_fullmac_impl_ifc_banjo_protocol_->ctx,
                                                        &auth_ind);
  }
  completer.Reply();
}

void Device::DeauthConf(DeauthConfRequestView request, DeauthConfCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    ZX_ASSERT(request->has_peer_sta_address());
    ZX_ASSERT(request->peer_sta_address().size() == ETH_ALEN);
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->deauth_conf(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, request->peer_sta_address().data());
  }
  completer.Reply();
}

void Device::DeauthInd(DeauthIndRequestView request, DeauthIndCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_deauth_indication_t deauth_ind;
    ConvertDeauthInd(request->ind, &deauth_ind);
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->deauth_ind(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, &deauth_ind);
  }
  completer.Reply();
}

void Device::AssocInd(AssocIndRequestView request, AssocIndCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_assoc_ind_t assoc_ind;
    ConvertAssocInd(request->resp, &assoc_ind);
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->assoc_ind(wlan_fullmac_impl_ifc_banjo_protocol_->ctx,
                                                         &assoc_ind);
  }
  completer.Reply();
}

void Device::DisassocConf(DisassocConfRequestView request, DisassocConfCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_disassoc_confirm_t disassoc_conf;
    disassoc_conf.status = request->resp.status;
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->disassoc_conf(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, &disassoc_conf);
  }
  completer.Reply();
}

void Device::DisassocInd(DisassocIndRequestView request, DisassocIndCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_disassoc_indication_t disassoc_ind;
    ConvertDisassocInd(request->ind, &disassoc_ind);
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->disassoc_ind(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, &disassoc_ind);
  }
  completer.Reply();
}

void Device::StartConf(StartConfRequestView request, StartConfCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_start_confirm_t start_conf;
    start_conf.result_code = ConvertStartResultCode(request->resp.result_code);
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->start_conf(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, &start_conf);
  }
  completer.Reply();
}

void Device::StopConf(StopConfRequestView request, StopConfCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_stop_confirm_t stop_conf;
    stop_conf.result_code = ConvertStopResultCode(request->resp.result_code);
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->stop_conf(wlan_fullmac_impl_ifc_banjo_protocol_->ctx,
                                                         &stop_conf);
  }
  completer.Reply();
}

void Device::EapolConf(EapolConfRequestView request, EapolConfCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_eapol_confirm_t eapol_conf;
    ConvertEapolConf(request->resp, &eapol_conf);
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->eapol_conf(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, &eapol_conf);
  }
  completer.Reply();
}

void Device::OnChannelSwitch(OnChannelSwitchRequestView request,
                             OnChannelSwitchCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_channel_switch_info channel_switch_info;
    channel_switch_info.new_channel = request->ind.new_channel;
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->on_channel_switch(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, &channel_switch_info);
  }
  completer.Reply();
}

void Device::SignalReport(SignalReportRequestView request, SignalReportCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_signal_report_indication_t signal_report_ind;
    signal_report_ind.rssi_dbm = request->ind.rssi_dbm;
    signal_report_ind.snr_db = request->ind.snr_db;
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->signal_report(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, &signal_report_ind);
  }
  completer.Reply();
}

void Device::EapolInd(EapolIndRequestView request, EapolIndCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_eapol_indication_t eapol_ind;
    ConvertEapolIndication(request->ind, &eapol_ind);
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->eapol_ind(wlan_fullmac_impl_ifc_banjo_protocol_->ctx,
                                                         &eapol_ind);
  }
  completer.Reply();
}

void Device::OnPmkAvailable(OnPmkAvailableRequestView request,
                            OnPmkAvailableCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_pmk_info_t pmk_info;
    pmk_info.pmk_list = request->info.pmk.data();
    pmk_info.pmk_count = request->info.pmk.count();
    pmk_info.pmkid_list = request->info.pmkid.data();
    pmk_info.pmkid_count = request->info.pmkid.count();
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->on_pmk_available(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, &pmk_info);
  }
  completer.Reply();
}

void Device::SaeHandshakeInd(SaeHandshakeIndRequestView request,
                             SaeHandshakeIndCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_sae_handshake_ind_t sae_handshake_ind;
    memcpy(sae_handshake_ind.peer_sta_address, request->ind.peer_sta_address.data(), ETH_ALEN);
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->sae_handshake_ind(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, &sae_handshake_ind);
  }
  completer.Reply();
}

void Device::SaeFrameRx(SaeFrameRxRequestView request, SaeFrameRxCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_sae_frame_t sae_frame;
    ConvertSaeFrame(request->frame, &sae_frame);
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->sae_frame_rx(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, &sae_frame);
  }
  completer.Reply();
}

void Device::OnWmmStatusResp(OnWmmStatusRespRequestView request,
                             OnWmmStatusRespCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_wmm_parameters_t wmm_params;
    ConvertWmmParams(request->wmm_params, &wmm_params);
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->on_wmm_status_resp(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, request->status, &wmm_params);
  }
  completer.Reply();
}

}  // namespace wlanif
FUCHSIA_DRIVER_EXPORT(::wlanif::Device);
