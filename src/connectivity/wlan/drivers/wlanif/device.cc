// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "device.h"

#include <fuchsia/hardware/wlan/fullmac/c/banjo.h>
#include <fuchsia/wlan/ieee80211/cpp/fidl.h>
#include <fuchsia/wlan/internal/c/banjo.h>
#include <fuchsia/wlan/internal/cpp/fidl.h>
#include <fuchsia/wlan/mlme/cpp/fidl.h>
#include <fuchsia/wlan/stats/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/ddk/device.h>
#include <net/ethernet.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include <memory>

#include <wlan/common/ieee80211_codes.h>
#include <wlan/drivers/log.h>

#include "convert.h"
#include "debug.h"
#include "driver.h"
#include "fuchsia/wlan/common/c/banjo.h"
#include "fuchsia/wlan/common/cpp/fidl.h"
#include "zircon/system/public/zircon/assert.h"

namespace wlanif {

namespace wlan_common = ::fuchsia::wlan::common;
namespace wlan_ieee80211 = ::fuchsia::wlan::ieee80211;
namespace wlan_mlme = ::fuchsia::wlan::mlme;
namespace wlan_stats = ::fuchsia::wlan::stats;

Device::Device(zx_device_t* device) : ddk::Device<Device, ddk::Unbindable>(device) {
  ltrace_fn();

  // Create a dispatcher to wait on the runtime channel
  auto dispatcher =
      fdf::SynchronizedDispatcher::Create(fdf::SynchronizedDispatcher::Options::kAllowSyncCalls,
                                          "wlan_fullmac_ifc_server", [&](fdf_dispatcher_t*) {
                                            if (unbind_txn_) {
                                              unbind_txn_->Reply();
                                              unbind_txn_.reset();
                                            }
                                          });

  if (dispatcher.is_error()) {
    lerror("Creating server dispatcher error : %s\n",
           zx_status_get_string(dispatcher.status_value()));
  }

  server_dispatcher_ = *std::move(dispatcher);

  // Establish the connection among histogram data lists, assuming each histogram list only contains
  // one histogram for now, will assert it in GetIfaceHistogramStats();
  noise_floor_histograms_.noise_floor_samples_list = noise_floor_buckets_;
  rssi_histograms_.rssi_samples_list = rssi_buckets_;
  rx_rate_index_histograms_.rx_rate_index_samples_list = rx_rate_index_buckets_;
  snr_histograms_.snr_samples_list = snr_buckets_;
}

// Reserve this version of constructor for testing purpose.
Device::Device(zx_device_t* device, fdf::ClientEnd<fuchsia_wlan_fullmac::WlanFullmacImpl> client)
    : ddk::Device<Device, ddk::Unbindable>(device) {
  // Create a dispatcher to wait on the runtime channel
  auto dispatcher =
      fdf::SynchronizedDispatcher::Create(fdf::SynchronizedDispatcher::Options::kAllowSyncCalls,
                                          "wlan_fullmac_ifc_server", [&](fdf_dispatcher_t*) {
                                            if (unbind_txn_) {
                                              unbind_txn_->Reply();
                                              unbind_txn_.reset();
                                            }
                                          });

  if (dispatcher.is_error()) {
    lerror("Creating server dispatcher error : %s\n",
           zx_status_get_string(dispatcher.status_value()));
  }

  server_dispatcher_ = *std::move(dispatcher);
  client_ = fdf::WireSyncClient<fuchsia_wlan_fullmac::WlanFullmacImpl>(std::move(client));

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
  mlme_->Init();
}

zx_status_t Device::AddDevice() { return DdkAdd(::ddk::DeviceAddArgs("wlanif")); }

zx_status_t Device::Bind() {
  ltrace_fn();
  // The MLME interface has no start/stop commands, so we will start the wlan_fullmac_impl
  // device immediately

  // Connect to the device which serves WlanFullmacImpl protocol service.
  zx_status_t status;

  auto mac_sublayer_arena = fdf::Arena::Create(0, 0);
  if (mac_sublayer_arena.is_error()) {
    lerror("Mac Sublayer arena creation failed: %s", mac_sublayer_arena.status_string());
    return ZX_ERR_INTERNAL;
  }

  auto mac_sublayer_result = client_.buffer(*mac_sublayer_arena)->QueryMacSublayerSupport();

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

  status = AddDevice();
  if (status != ZX_OK) {
    lerror("could not add ethernet_impl device: %s\n", zx_status_get_string(status));
  }

  return status;
}

zx_status_t Device::ConnectToWlanFullmacImpl() {
  zx::result<fdf::ClientEnd<fuchsia_wlan_fullmac::WlanFullmacImpl>> client_end =
      DdkConnectRuntimeProtocol<fuchsia_wlan_fullmac::Service::WlanFullmacImpl>();
  if (client_end.is_error()) {
    lerror("DDdkConnectRuntimeProtocol failed: %s", client_end.status_string());
    return client_end.status_value();
  }
  client_ = fdf::WireSyncClient(*std::move(client_end));
  return ZX_OK;
}

void Device::DdkUnbind(::ddk::UnbindTxn txn) {
  ltrace_fn();
  unbind_txn_ = std::move(txn);
  if (mlme_) {
    mlme_->StopMainLoop();
  }
  server_dispatcher_.ShutdownAsync();
}

void Device::DdkRelease() {
  ltrace_fn();
  delete this;
}

zx_status_t Device::Start(const rust_wlan_fullmac_ifc_protocol_copy_t* ifc,
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

  auto start_result = client_.buffer(*arena)->Start(std::move(endpoints->client));

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

void Device::StartScan(const wlan_fullmac_scan_req_t* req) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return;
  }
  fuchsia_wlan_fullmac::wire::WlanFullmacImplStartScanRequest scan_req;
  ConvertScanReq(*req, &scan_req, *arena);

  auto result = client_.buffer(*arena)->StartScan(scan_req);

  if (!result.ok()) {
    lerror("StartScan failed, FIDL error: %s", result.status_string());
    return;
  }
}

void Device::ConnectReq(const wlan_fullmac_connect_req_t* req) {
  OnLinkStateChanged(false);
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return;
  }

  fuchsia_wlan_fullmac::wire::WlanFullmacImplConnectReqRequest connect_req;
  ConvertConnectReq(*req, &connect_req, *arena);

  auto result = client_.buffer(*arena)->ConnectReq(connect_req);

  if (!result.ok()) {
    lerror("ConnectReq failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::ReconnectReq(const wlan_fullmac_reconnect_req_t* req) {
  fuchsia_wlan_fullmac::wire::WlanFullmacReconnectReq reconnect_req;

  // peer_sta_address
  std::memcpy(reconnect_req.peer_sta_address.data(), req->peer_sta_address, ETH_ALEN);

  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return;
  }

  auto result = client_.buffer(*arena)->ReconnectReq(reconnect_req);

  if (!result.ok()) {
    lerror("ReconnectReq failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::AuthenticateResp(const wlan_fullmac_auth_resp_t* resp) {
  fuchsia_wlan_fullmac::wire::WlanFullmacAuthResp auth_resp;

  // peer_sta_address
  std::memcpy(auth_resp.peer_sta_address.data(), resp->peer_sta_address, ETH_ALEN);

  // result_code
  auth_resp.result_code = ConvertAuthResult(resp->result_code);

  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return;
  }

  auto result = client_.buffer(*arena)->AuthResp(auth_resp);

  if (!result.ok()) {
    lerror("AuthResp failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::DeauthenticateReq(const wlan_fullmac_deauth_req_t* req) {
  OnLinkStateChanged(false);
  fuchsia_wlan_fullmac::wire::WlanFullmacDeauthReq deauth_req = {};

  // peer_sta_address
  std::memcpy(deauth_req.peer_sta_address.data(), req->peer_sta_address, ETH_ALEN);

  // reason_code
  deauth_req.reason_code = ConvertReasonCode(req->reason_code);

  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return;
  }

  auto result = client_.buffer(*arena)->DeauthReq(deauth_req);

  if (!result.ok()) {
    lerror("DeauthReq failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::AssociateResp(const wlan_fullmac_assoc_resp_t* resp) {
  fuchsia_wlan_fullmac::wire::WlanFullmacAssocResp assoc_resp;

  std::memcpy(assoc_resp.peer_sta_address.data(), resp->peer_sta_address, ETH_ALEN);
  assoc_resp.result_code = ConvertAssocResult(resp->result_code);
  assoc_resp.association_id = resp->association_id;

  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return;
  }

  auto result = client_.buffer(*arena)->AssocResp(assoc_resp);

  if (!result.ok()) {
    lerror("AssocResp failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::DisassociateReq(const wlan_fullmac_disassoc_req_t* req) {
  OnLinkStateChanged(false);

  fuchsia_wlan_fullmac::wire::WlanFullmacDisassocReq disassoc_req;

  std::memcpy(disassoc_req.peer_sta_address.data(), req->peer_sta_address, ETH_ALEN);
  disassoc_req.reason_code = ConvertReasonCode(req->reason_code);

  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return;
  }

  auto result = client_.buffer(*arena)->DisassocReq(disassoc_req);

  if (!result.ok()) {
    lerror("DisassocReq failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::ResetReq(const wlan_fullmac_reset_req_t* req) {
  OnLinkStateChanged(false);

  fuchsia_wlan_fullmac::wire::WlanFullmacResetReq reset_req;

  std::memcpy(reset_req.sta_address.data(), req->sta_address, ETH_ALEN);
  reset_req.set_default_mib = req->set_default_mib;

  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return;
  }

  auto result = client_.buffer(*arena)->ResetReq(reset_req);

  if (!result.ok()) {
    lerror("ResetReq failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::StartReq(const wlan_fullmac_start_req_t* req) {
  fuchsia_wlan_fullmac::wire::WlanFullmacStartReq start_req;

  ConvertCSsid(req->ssid, &start_req.ssid);
  start_req.bss_type = ConvertBssType(req->bss_type);
  start_req.beacon_period = req->beacon_period;
  start_req.dtim_period = req->dtim_period;
  start_req.channel = req->channel;
  std::memcpy(start_req.rsne.data(), req->rsne, req->rsne_len);
  start_req.rsne_len = req->rsne_len;
  std::memcpy(start_req.vendor_ie.data(), req->vendor_ie, req->vendor_ie_len);
  start_req.vendor_ie_len = req->vendor_ie_len;

  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return;
  }

  auto result = client_.buffer(*arena)->StartReq(start_req);

  if (!result.ok()) {
    lerror("StartReq failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::StopReq(const wlan_fullmac_stop_req_t* req) {
  fuchsia_wlan_fullmac::wire::WlanFullmacStopReq stop_req;
  ConvertCSsid(req->ssid, &stop_req.ssid);

  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return;
  }

  auto result = client_.buffer(*arena)->StopReq(stop_req);

  if (!result.ok()) {
    lerror("StopReq failed FIDL error: %s", result.status_string());
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

  // Initialize all the WlanKeyTypes to pass FIDL check.
  for (size_t i = 0; i < fuchsia_wlan_fullmac::wire::kWlanMaxKeylistSize; i++) {
    set_keys_req.keylist.data()[i].key_type = fuchsia_wlan_common::wire::WlanKeyType::kPairwise;
  }

  for (size_t desc_ndx = 0; desc_ndx < num_keys; desc_ndx++) {
    ConvertSetKeyDescriptor(req->keylist[desc_ndx], &set_keys_req.keylist.data()[desc_ndx], *arena);
  }

  auto result = client_.buffer(*arena)->SetKeysReq(set_keys_req);

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
    lwarn("truncating key list from %zu to %d members\n", num_keys,
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

    auto result = client_.buffer(*arena)->DelKeysReq(delete_key_req);

    if (!result.ok()) {
      lerror("DelKeysReq failed FIDL error: %s", result.status_string());
      return;
    }
  }
}

void Device::EapolReq(const wlan_fullmac_eapol_req_t* req) {
  fuchsia_wlan_fullmac::wire::WlanFullmacEapolReq eapol_req = {};

  std::memcpy(eapol_req.src_addr.data(), req->src_addr, ETH_ALEN);
  std::memcpy(eapol_req.dst_addr.data(), req->dst_addr, ETH_ALEN);
  auto data = std::vector<uint8_t>(req->data_list, req->data_list + req->data_count);

  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return;
  }
  eapol_req.data = fidl::VectorView<uint8_t>(*arena, data);

  auto result = client_.buffer(*arena)->EapolReq(eapol_req);

  if (!result.ok()) {
    lerror("EapolReq failed FIDL error: %s", result.status_string());
    return;
  }
}

void Device::QueryDeviceInfo(wlan_fullmac_query_info_t* out_resp) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return;
  }

  auto result = client_.buffer(*arena)->Query();

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

  auto result = client_.buffer(*arena)->QueryMacSublayerSupport();

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

  auto result = client_.buffer(*arena)->QuerySecuritySupport();

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

  auto result = client_.buffer(*arena)->QuerySpectrumManagementSupport();

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

  auto result = client_.buffer(*arena)->GetIfaceCounterStats();

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

  auto result = client_.buffer(*arena)->GetIfaceHistogramStats();

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

  // TODO(fxbug.dev/51009): Let SME handle these changes.
  if (online != device_online_) {
    device_online_ = online;
    auto result = client_.buffer(*arena)->OnLinkStateChanged(online);

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

  auto result = client_.buffer(*arena)->SaeHandshakeResp(handshake_resp);

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

  auto result = client_.buffer(*arena)->SaeFrameTx(sae_frame);

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

  auto result = client_.buffer(*arena)->WmmStatusReq();

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
  wlan_fullmac_deauth_confirm_t deauth_conf;
  memcpy(deauth_conf.peer_sta_address, request->resp.peer_sta_address.data(), ETH_ALEN);
  wlan_fullmac_impl_ifc_protocol_ops_->deauth_conf(wlan_fullmac_impl_ifc_protocol_->ctx,
                                                   &deauth_conf);
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

void Device::RelayCapturedFrame(RelayCapturedFrameRequestView request, fdf::Arena& arena,
                                RelayCapturedFrameCompleter::Sync& completer) {
  wlan_fullmac_captured_frame_result_t captured_frame_result;
  captured_frame_result.data_list = request->result.data.data();
  captured_frame_result.data_count = request->result.data.count();
  wlan_fullmac_impl_ifc_protocol_ops_->relay_captured_frame(wlan_fullmac_impl_ifc_protocol_->ctx,
                                                            &captured_frame_result);
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
