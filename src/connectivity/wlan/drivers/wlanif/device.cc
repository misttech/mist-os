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
#include "wlan/drivers/log_instance.h"
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
  wlan::drivers::log::Instance::Init(0);
  ltrace_fn();
  // Establish the connection among histogram data lists, assuming each histogram list only contains
  // one histogram for now, will assert it in GetIfaceHistogramStats();
  noise_floor_histograms_.noise_floor_samples_list = noise_floor_buckets_;
  rssi_histograms_.rssi_samples_list = rssi_buckets_;
  rx_rate_index_histograms_.rx_rate_index_samples_list = rx_rate_index_buckets_;
  snr_histograms_.snr_samples_list = snr_buckets_;
}

Device::~Device() { ltrace_fn(); }

void Device::InitMlme() {
  mlme_ = std::make_unique<FullmacMlme>(this);
  ZX_DEBUG_ASSERT(mlme_ != nullptr);
  if (mlme_->Init() != ZX_OK) {
    mlme_.reset();
  }
}

zx::result<> Device::Start() {
  {
    // Create a dispatcher to wait on the runtime channel
    auto dispatcher =
        fdf::SynchronizedDispatcher::Create(fdf::SynchronizedDispatcher::Options::kAllowSyncCalls,
                                            "wlan_fullmac_ifc_server", [&](fdf_dispatcher_t*) {});

    if (dispatcher.is_error()) {
      lerror("Creating server dispatcher error : %s",
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
      lerror("Creating client dispatcher error : %s",
             zx_status_get_string(dispatcher.status_value()));
      return zx::error(dispatcher.status_value());
    }
    client_dispatcher_ = *std::move(dispatcher);
  }
  auto endpoints = fdf::CreateEndpoints<fuchsia_wlan_fullmac::WlanFullmacImpl>();
  if (endpoints.is_error()) {
    lerror("Creating end point error: %s", zx_status_get_string(endpoints.status_value()));
    return zx::error(endpoints.status_value());
  }

  zx_status_t status;
  if ((status = ConnectToWlanFullmacImpl()) != ZX_OK) {
    lerror("Failed connecting to wlan fullmac impl driver: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  if ((status = Bind()) != ZX_OK) {
    lerror("Failed adding wlan fullmac device: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  return zx::ok();
}

void Device::PrepareStop(fdf::PrepareStopCompleter completer) {
  client_.AsyncTeardown();
  if (mlme_.get()) {
    mlme_->StopMainLoop();
  }
  completer(zx::ok());
}

zx_status_t Device::Bind() {
  ltrace_fn();
  // The MLME interface has no start/stop commands, so we will start the wlan_fullmac_impl
  // device immediately

  // Connect to the device which serves WlanFullmacImpl protocol service.
  zx_status_t status = ZX_OK;

  auto mac_sublayer_arena = fdf::Arena::Create(0, 0);
  if (mac_sublayer_arena.is_error()) {
    lerror("Mac Sublayer arena creation failed: %s", mac_sublayer_arena.status_string());
    return ZX_ERR_INTERNAL;
  }

  auto mac_sublayer_result = client_.sync().buffer(*mac_sublayer_arena)->QueryMacSublayerSupport();

  if (!mac_sublayer_result.ok()) {
    lerror("QueryMacSublayerSupport failed  FIDL error: %s", mac_sublayer_result.status_string());
    return mac_sublayer_result.status();
  }
  if (mac_sublayer_result->is_error()) {
    lerror("QueryMacSublayerSupport failed : %s",
           zx_status_get_string(mac_sublayer_result->error_value()));
    return mac_sublayer_result->error_value();
  }

  if (mac_sublayer_result->value()->resp.data_plane.data_plane_type ==
      fuchsia_wlan_common::wire::DataPlaneType::kEthernetDevice) {
    lerror("Ethernet data plane is no longer supported by wlanif");
    return ZX_ERR_NOT_SUPPORTED;
  }

  InitMlme();
  return status;
}

zx_status_t Device::ConnectToWlanFullmacImpl() {
  zx::result<fdf::ClientEnd<fuchsia_wlan_fullmac::WlanFullmacImpl>> client_end =
      incoming()->Connect<fuchsia_wlan_fullmac::Service::WlanFullmacImpl>();
  if (client_end.is_error()) {
    lerror("Connect to FullmacImpl failed: %s", client_end.status_string());
    return client_end.status_value();
  }

  client_ = fdf::WireSharedClient(std::move(client_end.value()), client_dispatcher_.get());
  if (!client_.is_valid()) {
    lerror("WlanFullmacImpl Client is not valid");
    return ZX_ERR_BAD_HANDLE;
  }
  return ZX_OK;
}

zx_status_t Device::StartFullmac(const rust_wlan_fullmac_ifc_protocol_copy_t* ifc,
                                 zx::channel* out_sme_channel) {
  // We manually populate the protocol ops here so that we can verify at compile time that our rust
  // bindings have the expected parameters.
  wlan_fullmac_impl_ifc_protocol_ops_.reset(new wlan_fullmac_impl_ifc_protocol_ops_t{
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

  wlan_fullmac_impl_ifc_protocol_ = std::make_unique<wlan_fullmac_impl_ifc_protocol_t>();
  wlan_fullmac_impl_ifc_protocol_->ops = wlan_fullmac_impl_ifc_protocol_ops_.get();
  wlan_fullmac_impl_ifc_protocol_->ctx = ifc->ctx;

  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return ZX_ERR_INTERNAL;
  }

  auto endpoints = fdf::CreateEndpoints<fuchsia_wlan_fullmac::WlanFullmacImplIfc>();
  if (endpoints.is_error()) {
    lerror("Creating endpoints error: %s", zx_status_get_string(endpoints.status_value()));
    return endpoints.status_value();
  }

  fdf::BindServer(server_dispatcher_.get(), std::move(endpoints->server), this);

  auto start_result = client_.sync().buffer(*arena)->Start(std::move(endpoints->client));

  if (!start_result.ok()) {
    lerror("Start failed, FIDL error: %s", start_result.status_string());
    return start_result.status();
  }

  if (start_result->is_error()) {
    lerror("Start failed: %s", zx_status_get_string(start_result->error_value()));
    return start_result->error_value();
  }

  *out_sme_channel = std::move(start_result->value()->sme_channel);
  return ZX_OK;
}

void Device::StartScan(const wlan_fullmac_impl_start_scan_request_t* req) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return;
  }
  fuchsia_wlan_fullmac::wire::WlanFullmacImplStartScanRequest scan_req;
  ConvertScanReq(*req, &scan_req, *arena);

  auto result = client_.sync().buffer(*arena)->StartScan(scan_req);

  if (!result.ok()) {
    lerror("StartScan failed, FIDL error: %s", result.status_string());
    return;
  }
}

void Device::Connect(const wlan_fullmac_impl_connect_request_t* req) {
  OnLinkStateChanged(false);
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return;
  }

  fuchsia_wlan_fullmac::wire::WlanFullmacImplConnectRequest connect_req;
  ConvertConnectReq(*req, &connect_req, *arena);

  auto result = client_.sync().buffer(*arena)->Connect(connect_req);

  if (!result.ok()) {
    lerror("ConnectReq failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::Reconnect(const wlan_fullmac_impl_reconnect_request_t* req) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return;
  }
  auto builder = fuchsia_wlan_fullmac::wire::WlanFullmacImplReconnectRequest::Builder(*arena);
  // peer_sta_address
  ::fidl::Array<uint8_t, ETH_ALEN> peer_sta_address;
  std::memcpy(peer_sta_address.data(), req->peer_sta_address, ETH_ALEN);
  builder.peer_sta_address(peer_sta_address);

  auto result = client_.sync().buffer(*arena)->Reconnect(builder.Build());

  if (!result.ok()) {
    lerror("ReconnectReq failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::AuthenticateResp(const wlan_fullmac_impl_auth_resp_request_t* resp) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return;
  }

  auto builder = fuchsia_wlan_fullmac::wire::WlanFullmacImplAuthRespRequest::Builder(*arena);
  // peer_sta_address
  ::fidl::Array<uint8_t, ETH_ALEN> peer_sta_address;
  std::memcpy(peer_sta_address.data(), resp->peer_sta_address, ETH_ALEN);
  builder.peer_sta_address(peer_sta_address);

  // result_code
  builder.result_code(ConvertAuthResult(resp->result_code));

  auto result = client_.sync().buffer(*arena)->AuthResp(builder.Build());

  if (!result.ok()) {
    lerror("AuthResp failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::Deauthenticate(const wlan_fullmac_impl_deauth_request_t* req) {
  OnLinkStateChanged(false);
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return;
  }
  auto builder = fuchsia_wlan_fullmac::wire::WlanFullmacImplDeauthRequest::Builder(*arena);

  ::fidl::Array<uint8_t, ETH_ALEN> peer_sta_address;
  std::memcpy(peer_sta_address.data(), req->peer_sta_address, ETH_ALEN);
  builder.peer_sta_address(peer_sta_address);
  builder.reason_code(fuchsia_wlan_ieee80211::ReasonCode(req->reason_code));

  auto result = client_.sync().buffer(*arena)->Deauth(builder.Build());

  if (!result.ok()) {
    lerror("DeauthReq failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::AssociateResp(const wlan_fullmac_impl_assoc_resp_request_t* resp) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return;
  }
  auto builder = fuchsia_wlan_fullmac::wire::WlanFullmacImplAssocRespRequest::Builder(*arena);
  ::fidl::Array<uint8_t, ETH_ALEN> peer_sta_address;
  std::memcpy(peer_sta_address.data(), resp->peer_sta_address, ETH_ALEN);
  builder.peer_sta_address(peer_sta_address);
  builder.result_code(ConvertAssocResult(resp->result_code));
  builder.association_id(resp->association_id);

  auto result = client_.sync().buffer(*arena)->AssocResp(builder.Build());

  if (!result.ok()) {
    lerror("AssocResp failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::Disassociate(const wlan_fullmac_impl_disassoc_request_t* req) {
  OnLinkStateChanged(false);
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return;
  }

  auto builder = fuchsia_wlan_fullmac::wire::WlanFullmacImplDisassocRequest::Builder(*arena);
  ::fidl::Array<uint8_t, ETH_ALEN> peer_sta_address;
  std::memcpy(peer_sta_address.data(), req->peer_sta_address, ETH_ALEN);
  builder.peer_sta_address(peer_sta_address);
  builder.reason_code(static_cast<fuchsia_wlan_ieee80211::wire::ReasonCode>(req->reason_code));

  auto result = client_.sync().buffer(*arena)->Disassoc(builder.Build());

  if (!result.ok()) {
    lerror("DisassocReq failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::Reset(const wlan_fullmac_impl_reset_request_t* req) {
  OnLinkStateChanged(false);

  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return;
  }
  auto builder = fuchsia_wlan_fullmac::wire::WlanFullmacImplResetRequest::Builder(*arena);

  ::fidl::Array<uint8_t, ETH_ALEN> sta_address;
  std::memcpy(sta_address.data(), req->sta_address, ETH_ALEN);
  builder.sta_address(sta_address);
  builder.set_default_mib(req->set_default_mib);

  auto result = client_.sync().buffer(*arena)->Reset(builder.Build());
  if (!result.ok()) {
    lerror("ResetReq failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::StartBss(const wlan_fullmac_impl_start_bss_request_t* req) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
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

  auto result = client_.sync().buffer(*arena)->StartBss(builder.Build());

  if (!result.ok()) {
    lerror("StartBss failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::StopBss(const wlan_fullmac_impl_stop_bss_request_t* req) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return;
  }
  auto builder = fuchsia_wlan_fullmac::wire::WlanFullmacImplStopBssRequest::Builder(*arena);
  fuchsia_wlan_ieee80211::wire::CSsid ssid;
  ConvertCSsid(req->ssid, &ssid);
  builder.ssid(ssid);

  auto result = client_.sync().buffer(*arena)->StopBss(builder.Build());

  if (!result.ok()) {
    lerror("StopBss failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::SetKeysReq(const wlan_fullmac_set_keys_req_t* req,
                        wlan_fullmac_set_keys_resp_t* out_resp) {
  fuchsia_wlan_fullmac::wire::WlanFullmacSetKeysReq set_keys_req;

  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return;
  }

  // Abort if too many keys sent.
  size_t num_keys = req->num_keys;
  if (num_keys > fuchsia_wlan_fullmac::wire::kWlanMaxKeylistSize) {
    lerror("Key list with length %zu exceeds maximum size %d", num_keys, WLAN_MAX_KEYLIST_SIZE);
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

  auto result = client_.sync().buffer(*arena)->SetKeysReq(set_keys_req);

  if (!result.ok()) {
    lerror("SetKeyReq failed FIDL error: %s", result.status_string());
    return;
  }

  auto set_key_resp = result->resp;

  auto num_results = set_key_resp.num_keys;
  if (num_keys != num_results) {
    lerror("SetKeysReq count (%zu) and SetKeyResp count (%zu) do not agree", num_keys, num_results);
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

    auto arena = fdf::Arena::Create(0, 0);
    if (arena.is_error()) {
      lerror("Arena creation failed: %s", arena.status_string());
      return;
    }

    auto result = client_.sync().buffer(*arena)->DelKeysReq(delete_key_req);

    if (!result.ok()) {
      lerror("DelKeysReq failed FIDL error: %s", result.status_string());
      return;
    }
  }
}

void Device::EapolTx(const wlan_fullmac_impl_eapol_tx_request_t* req) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
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

  auto result = client_.sync().buffer(*arena)->EapolTx(eapol_req);

  if (!result.ok()) {
    lerror("EapolTx failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::QueryDeviceInfo(wlan_fullmac_query_info_t* out_resp) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return;
  }

  auto result = client_.sync().buffer(*arena)->Query();

  if (!result.ok()) {
    lerror("Query failed FIDL error: %s", result.status_string());
    return;
  }
  if (result->is_error()) {
    lerror("Query failed : %s", zx_status_get_string(result->error_value()));
    return;
  }

  auto& query_info = result->value()->info;
  ConvertQueryInfo(query_info, out_resp);
}

void Device::QueryMacSublayerSupport(mac_sublayer_support_t* out_resp) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return;
  }

  auto result = client_.sync().buffer(*arena)->QueryMacSublayerSupport();

  if (!result.ok()) {
    lerror("Query failed FIDL error: %s", result.status_string());
    return;
  }
  if (result->is_error()) {
    lerror("Query failed : %s", zx_status_get_string(result->error_value()));
    return;
  }
  auto& mac_sublayer_support = result->value()->resp;
  ConvertMacSublayerSupport(mac_sublayer_support, out_resp);
}

void Device::QuerySecuritySupport(security_support_t* out_resp) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return;
  }

  auto result = client_.sync().buffer(*arena)->QuerySecuritySupport();

  if (!result.ok()) {
    lerror("QuerySecuritySupport failed FIDL error: %s", result.status_string());
    return;
  }
  if (result->is_error()) {
    lerror("QuerySecuritySupport failed : %s", zx_status_get_string(result->error_value()));
    return;
  }

  auto& security_support = result->value()->resp;
  ConvertSecuritySupport(security_support, out_resp);
}

void Device::QuerySpectrumManagementSupport(spectrum_management_support_t* out_resp) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return;
  }

  auto result = client_.sync().buffer(*arena)->QuerySpectrumManagementSupport();

  if (!result.ok()) {
    lerror("QuerySpectrumManagementSupport failed FIDL error: %s", result.status_string());
    return;
  }
  if (result->is_error()) {
    lerror("QuerySpectrumManagementSupport failed : %s",
           zx_status_get_string(result->error_value()));
    return;
  }

  auto& spectrum_management_support = result->value()->resp;
  ConvertSpectrumManagementSupport(spectrum_management_support, out_resp);
}

zx_status_t Device::GetIfaceCounterStats(wlan_fullmac_iface_counter_stats_t* out_stats) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return arena.status_value();
  }

  auto result = client_.sync().buffer(*arena)->GetIfaceCounterStats();

  if (!result.ok()) {
    lerror("GetIfaceCounterStats failed FIDL error: %s", result.status_string());
    return result.status();
  }
  if (result->is_error()) {
    lerror("GetIfaceCounterStats failed : %s", zx_status_get_string(result->error_value()));
    return result->error_value();
  }
  auto& iface_counter_stats = result->value()->stats;
  ConvertIfaceCounterStats(iface_counter_stats, out_stats);
  return ZX_OK;
}

zx_status_t Device::GetIfaceHistogramStats(wlan_fullmac_iface_histogram_stats_t* out_stats) {
  std::lock_guard<std::mutex> lock(get_iface_histogram_stats_lock_);

  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return arena.status_value();
  }

  auto result = client_.sync().buffer(*arena)->GetIfaceHistogramStats();

  if (!result.ok()) {
    lerror("GetIfaceHistogramStats failed FIDL error: %s", result.status_string());
    return result.status();
  }
  if (result->is_error()) {
    lerror("GetIfaceHistogramStats failed : %s", zx_status_get_string(result->error_value()));
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
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return;
  }

  // TODO(https://fxbug.dev/51009): Let SME handle these changes.
  if (online != device_online_) {
    device_online_ = online;
    auto result = client_.sync().buffer(*arena)->OnLinkStateChanged(online);

    if (!result.ok()) {
      lerror("OnLinkStateChanged failed FIDL error: %s", result.status_string());
      return;
    }
  }
}

void Device::SaeHandshakeResp(const wlan_fullmac_sae_handshake_resp_t* resp) {
  fuchsia_wlan_fullmac::wire::WlanFullmacSaeHandshakeResp handshake_resp;

  ConvertSaeHandshakeResp(*resp, &handshake_resp);

  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return;
  }

  auto result = client_.sync().buffer(*arena)->SaeHandshakeResp(handshake_resp);

  if (!result.ok()) {
    lerror("SaeHandshakeResp failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::SaeFrameTx(const wlan_fullmac_sae_frame_t* frame) {
  fuchsia_wlan_fullmac::wire::WlanFullmacSaeFrame sae_frame;
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return;
  }

  ConvertSaeFrame(*frame, &sae_frame, *arena);

  auto result = client_.sync().buffer(*arena)->SaeFrameTx(sae_frame);

  if (!result.ok()) {
    lerror("SaeFrameTx failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::WmmStatusReq() {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return;
  }

  auto result = client_.sync().buffer(*arena)->WmmStatusReq();

  if (!result.ok()) {
    lerror("WmmStatusReq failed FIDL error: %s", result.status_string());
    return;
  }
}

// Implementation of fuchsia_wlan_fullmac::WlanFullmacImplIfc.
void Device::OnScanResult(OnScanResultRequestView request, fdf::Arena& arena,
                          OnScanResultCompleter::Sync& completer) {
  wlan_fullmac_scan_result_t scan_result;
  ConvertFullmacScanResult(request->result, &scan_result);
  wlan_fullmac_impl_ifc_protocol_ops_->on_scan_result(wlan_fullmac_impl_ifc_protocol_->ctx,
                                                      &scan_result);
  completer.buffer(arena).Reply();
}

void Device::OnScanEnd(OnScanEndRequestView request, fdf::Arena& arena,
                       OnScanEndCompleter::Sync& completer) {
  wlan_fullmac_scan_end_t scan_end;
  ConvertScanEnd(request->end, &scan_end);
  wlan_fullmac_impl_ifc_protocol_ops_->on_scan_end(wlan_fullmac_impl_ifc_protocol_->ctx, &scan_end);
  completer.buffer(arena).Reply();
}

void Device::ConnectConf(ConnectConfRequestView request, fdf::Arena& arena,
                         ConnectConfCompleter::Sync& completer) {
  wlan_fullmac_connect_confirm_t connect_conf;
  ConvertConnectConfirm(request->resp, &connect_conf);
  wlan_fullmac_impl_ifc_protocol_ops_->connect_conf(wlan_fullmac_impl_ifc_protocol_->ctx,
                                                    &connect_conf);
  completer.buffer(arena).Reply();
}

void Device::RoamConf(RoamConfRequestView request, fdf::Arena& arena,
                      RoamConfCompleter::Sync& completer) {
  wlan_fullmac_roam_confirm_t roam_conf;
  ConvertRoamConfirm(request->resp, &roam_conf);
  wlan_fullmac_impl_ifc_protocol_ops_->roam_conf(wlan_fullmac_impl_ifc_protocol_->ctx, &roam_conf);
  completer.buffer(arena).Reply();
}

void Device::AuthInd(AuthIndRequestView request, fdf::Arena& arena,
                     AuthIndCompleter::Sync& completer) {
  wlan_fullmac_auth_ind_t auth_ind;
  ConvertAuthInd(request->resp, &auth_ind);
  wlan_fullmac_impl_ifc_protocol_ops_->auth_ind(wlan_fullmac_impl_ifc_protocol_->ctx, &auth_ind);
  completer.buffer(arena).Reply();
}

void Device::DeauthConf(DeauthConfRequestView request, fdf::Arena& arena,
                        DeauthConfCompleter::Sync& completer) {
  ZX_ASSERT(request->has_peer_sta_address());
  ZX_ASSERT(request->peer_sta_address().size() == ETH_ALEN);
  wlan_fullmac_impl_ifc_protocol_ops_->deauth_conf(wlan_fullmac_impl_ifc_protocol_->ctx,
                                                   request->peer_sta_address().data());
  completer.buffer(arena).Reply();
}

void Device::DeauthInd(DeauthIndRequestView request, fdf::Arena& arena,
                       DeauthIndCompleter::Sync& completer) {
  wlan_fullmac_deauth_indication_t deauth_ind;
  ConvertDeauthInd(request->ind, &deauth_ind);
  wlan_fullmac_impl_ifc_protocol_ops_->deauth_ind(wlan_fullmac_impl_ifc_protocol_->ctx,
                                                  &deauth_ind);
  completer.buffer(arena).Reply();
}

void Device::AssocInd(AssocIndRequestView request, fdf::Arena& arena,
                      AssocIndCompleter::Sync& completer) {
  wlan_fullmac_assoc_ind_t assoc_ind;
  ConvertAssocInd(request->resp, &assoc_ind);
  wlan_fullmac_impl_ifc_protocol_ops_->assoc_ind(wlan_fullmac_impl_ifc_protocol_->ctx, &assoc_ind);
  completer.buffer(arena).Reply();
}

void Device::DisassocConf(DisassocConfRequestView request, fdf::Arena& arena,
                          DisassocConfCompleter::Sync& completer) {
  wlan_fullmac_disassoc_confirm_t disassoc_conf;
  disassoc_conf.status = request->resp.status;
  wlan_fullmac_impl_ifc_protocol_ops_->disassoc_conf(wlan_fullmac_impl_ifc_protocol_->ctx,
                                                     &disassoc_conf);
  completer.buffer(arena).Reply();
}

void Device::DisassocInd(DisassocIndRequestView request, fdf::Arena& arena,
                         DisassocIndCompleter::Sync& completer) {
  wlan_fullmac_disassoc_indication_t disassoc_ind;
  ConvertDisassocInd(request->ind, &disassoc_ind);
  wlan_fullmac_impl_ifc_protocol_ops_->disassoc_ind(wlan_fullmac_impl_ifc_protocol_->ctx,
                                                    &disassoc_ind);
  completer.buffer(arena).Reply();
}

void Device::StartConf(StartConfRequestView request, fdf::Arena& arena,
                       StartConfCompleter::Sync& completer) {
  wlan_fullmac_start_confirm_t start_conf;
  start_conf.result_code = ConvertStartResultCode(request->resp.result_code);
  wlan_fullmac_impl_ifc_protocol_ops_->start_conf(wlan_fullmac_impl_ifc_protocol_->ctx,
                                                  &start_conf);
  completer.buffer(arena).Reply();
}

void Device::StopConf(StopConfRequestView request, fdf::Arena& arena,
                      StopConfCompleter::Sync& completer) {
  wlan_fullmac_stop_confirm_t stop_conf;
  stop_conf.result_code = ConvertStopResultCode(request->resp.result_code);
  wlan_fullmac_impl_ifc_protocol_ops_->stop_conf(wlan_fullmac_impl_ifc_protocol_->ctx, &stop_conf);
  completer.buffer(arena).Reply();
}

void Device::EapolConf(EapolConfRequestView request, fdf::Arena& arena,
                       EapolConfCompleter::Sync& completer) {
  wlan_fullmac_eapol_confirm_t eapol_conf;
  ConvertEapolConf(request->resp, &eapol_conf);
  wlan_fullmac_impl_ifc_protocol_ops_->eapol_conf(wlan_fullmac_impl_ifc_protocol_->ctx,
                                                  &eapol_conf);
  completer.buffer(arena).Reply();
}

void Device::OnChannelSwitch(OnChannelSwitchRequestView request, fdf::Arena& arena,
                             OnChannelSwitchCompleter::Sync& completer) {
  wlan_fullmac_channel_switch_info channel_switch_info;
  channel_switch_info.new_channel = request->ind.new_channel;
  wlan_fullmac_impl_ifc_protocol_ops_->on_channel_switch(wlan_fullmac_impl_ifc_protocol_->ctx,
                                                         &channel_switch_info);
  completer.buffer(arena).Reply();
}

void Device::SignalReport(SignalReportRequestView request, fdf::Arena& arena,
                          SignalReportCompleter::Sync& completer) {
  wlan_fullmac_signal_report_indication_t signal_report_ind;
  signal_report_ind.rssi_dbm = request->ind.rssi_dbm;
  signal_report_ind.snr_db = request->ind.snr_db;
  wlan_fullmac_impl_ifc_protocol_ops_->signal_report(wlan_fullmac_impl_ifc_protocol_->ctx,
                                                     &signal_report_ind);
  completer.buffer(arena).Reply();
}

void Device::EapolInd(EapolIndRequestView request, fdf::Arena& arena,
                      EapolIndCompleter::Sync& completer) {
  wlan_fullmac_eapol_indication_t eapol_ind;
  ConvertEapolIndication(request->ind, &eapol_ind);
  wlan_fullmac_impl_ifc_protocol_ops_->eapol_ind(wlan_fullmac_impl_ifc_protocol_->ctx, &eapol_ind);
  completer.buffer(arena).Reply();
}

void Device::OnPmkAvailable(OnPmkAvailableRequestView request, fdf::Arena& arena,
                            OnPmkAvailableCompleter::Sync& completer) {
  wlan_fullmac_pmk_info_t pmk_info;
  pmk_info.pmk_list = request->info.pmk.data();
  pmk_info.pmk_count = request->info.pmk.count();
  pmk_info.pmkid_list = request->info.pmkid.data();
  pmk_info.pmkid_count = request->info.pmkid.count();
  wlan_fullmac_impl_ifc_protocol_ops_->on_pmk_available(wlan_fullmac_impl_ifc_protocol_->ctx,
                                                        &pmk_info);
  completer.buffer(arena).Reply();
}

void Device::SaeHandshakeInd(SaeHandshakeIndRequestView request, fdf::Arena& arena,
                             SaeHandshakeIndCompleter::Sync& completer) {
  wlan_fullmac_sae_handshake_ind_t sae_handshake_ind;
  memcpy(sae_handshake_ind.peer_sta_address, request->ind.peer_sta_address.data(), ETH_ALEN);
  wlan_fullmac_impl_ifc_protocol_ops_->sae_handshake_ind(wlan_fullmac_impl_ifc_protocol_->ctx,
                                                         &sae_handshake_ind);
  completer.buffer(arena).Reply();
}

void Device::SaeFrameRx(SaeFrameRxRequestView request, fdf::Arena& arena,
                        SaeFrameRxCompleter::Sync& completer) {
  wlan_fullmac_sae_frame_t sae_frame;
  ConvertSaeFrame(request->frame, &sae_frame);
  wlan_fullmac_impl_ifc_protocol_ops_->sae_frame_rx(wlan_fullmac_impl_ifc_protocol_->ctx,
                                                    &sae_frame);
  completer.buffer(arena).Reply();
}

void Device::OnWmmStatusResp(OnWmmStatusRespRequestView request, fdf::Arena& arena,
                             OnWmmStatusRespCompleter::Sync& completer) {
  wlan_wmm_parameters_t wmm_params;
  ConvertWmmParams(request->wmm_params, &wmm_params);
  wlan_fullmac_impl_ifc_protocol_ops_->on_wmm_status_resp(wlan_fullmac_impl_ifc_protocol_->ctx,
                                                          request->status, &wmm_params);
  completer.buffer(arena).Reply();
}

}  // namespace wlanif
FUCHSIA_DRIVER_EXPORT(::wlanif::Device);
