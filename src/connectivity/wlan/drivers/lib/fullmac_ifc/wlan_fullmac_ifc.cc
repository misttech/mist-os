// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/wlan/drivers/lib/fullmac_ifc/wlan_fullmac_ifc.h"

#include <lib/fdf/cpp/arena.h>
#include <zircon/types.h>

#include <memory>

#include <wlan/common/ieee80211_codes.h>
#include <wlan/drivers/log.h>

namespace wlan {

// WlanFullmacIfc implementations.
WlanFullmacIfc::WlanFullmacIfc(
    fdf::ClientEnd<fuchsia_wlan_fullmac::WlanFullmacImplIfc> client_end) {
  ifc_client_ =
      fdf::WireSyncClient<fuchsia_wlan_fullmac::WlanFullmacImplIfc>(std::move(client_end));
}

// This is not a deep copy.
void ConvertBssDescription(const bss_description_t& in,
                           fuchsia_wlan_internal::wire::BssDescription* out,
                           fidl::AnyArena& arena) {
  // BssDescription
  memcpy(out->bssid.data(), in.bssid, fuchsia_wlan_ieee80211::wire::kMacAddrLen);
  switch (in.bss_type) {
    case BSS_TYPE_UNKNOWN:
      out->bss_type = fuchsia_wlan_common::wire::BssType::kUnknown;
      break;
    case BSS_TYPE_INFRASTRUCTURE:
      out->bss_type = fuchsia_wlan_common::wire::BssType::kInfrastructure;
      break;
    case BSS_TYPE_INDEPENDENT:
      out->bss_type = fuchsia_wlan_common::wire::BssType::kIndependent;
      break;
    case BSS_TYPE_MESH:
      out->bss_type = fuchsia_wlan_common::wire::BssType::kMesh;
      break;
    case BSS_TYPE_PERSONAL:
      out->bss_type = fuchsia_wlan_common::wire::BssType::kPersonal;
      break;
    default:
      lerror("Unknown BssType: %hhu", in.bss_type);
      return;
  }

  out->beacon_period = in.beacon_period;
  out->capability_info = in.capability_info;
  if (in.ies_count > 0) {
    auto ie_vec = std::vector<uint8_t>(in.ies_list, in.ies_list + in.ies_count);
    out->ies = fidl::VectorView<uint8_t>(arena, ie_vec);
  }

  auto& channel = out->channel;
  channel.primary = in.channel.primary;
  channel.secondary80 = in.channel.secondary80;

  switch (in.channel.cbw) {
    case CHANNEL_BANDWIDTH_CBW20:
      channel.cbw = fuchsia_wlan_common::wire::ChannelBandwidth::kCbw20;
      break;
    case CHANNEL_BANDWIDTH_CBW40:
      channel.cbw = fuchsia_wlan_common::wire::ChannelBandwidth::kCbw40;
      break;
    case CHANNEL_BANDWIDTH_CBW40BELOW:
      channel.cbw = fuchsia_wlan_common::wire::ChannelBandwidth::kCbw40Below;
      break;
    case CHANNEL_BANDWIDTH_CBW80:
      channel.cbw = fuchsia_wlan_common::wire::ChannelBandwidth::kCbw80;
      break;
    case CHANNEL_BANDWIDTH_CBW160:
      channel.cbw = fuchsia_wlan_common::wire::ChannelBandwidth::kCbw160;
      break;
    case CHANNEL_BANDWIDTH_CBW80P80:
      channel.cbw = fuchsia_wlan_common::wire::ChannelBandwidth::kCbw80P80;
      break;
    default:
      lerror("Unknown BssType: %hhu", in.bss_type);
      return;
  }

  out->rssi_dbm = in.rssi_dbm;
  out->snr_db = in.snr_db;
}

// These functions convert banjo types into FIDL ones and send them up to wlanif driver.
void WlanFullmacIfc::OnScanResult(const wlan_fullmac_scan_result_t* res) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror(
        "Failed to create Arena in WlanFullmacIfc::OnScanResult(). "
        "status=%s\n",
        arena.status_string());
    return;
  }

  fuchsia_wlan_fullmac::wire::WlanFullmacScanResult fidl_result;

  fidl_result.txn_id = res->txn_id;
  fidl_result.timestamp_nanos = res->timestamp_nanos;

  ConvertBssDescription(res->bss, &fidl_result.bss, *arena);

  auto result = ifc_client_.buffer(*arena)->OnScanResult(fidl_result);
  if (!result.ok()) {
    lerror(
        "Failed to WlanScanResult up in WlanFullmacIfc::OnScanResult(). "
        "result.status: %s, txn_id=%zu\n",
        result.status_string(), res->txn_id);
    return;
  }
}

void WlanFullmacIfc::OnScanEnd(const wlan_fullmac_scan_end_t* end) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror(
        "Failed to create Arena in WlanFullmacIfc::OnScanEnd(). "
        "status=%s\n",
        arena.status_string());
    return;
  }

  fuchsia_wlan_fullmac::wire::WlanFullmacScanEnd fidl_end;

  fidl_end.txn_id = end->txn_id;
  switch (end->code) {
    case WLAN_SCAN_RESULT_SUCCESS:
      fidl_end.code = fuchsia_wlan_fullmac::wire::WlanScanResult::kSuccess;
      break;
    case WLAN_SCAN_RESULT_NOT_SUPPORTED:
      fidl_end.code = fuchsia_wlan_fullmac::wire::WlanScanResult::kNotSupported;
      break;
    case WLAN_SCAN_RESULT_INVALID_ARGS:
      fidl_end.code = fuchsia_wlan_fullmac::wire::WlanScanResult::kInvalidArgs;
      break;
    case WLAN_SCAN_RESULT_INTERNAL_ERROR:
      fidl_end.code = fuchsia_wlan_fullmac::wire::WlanScanResult::kInternalError;
      break;
    case WLAN_SCAN_RESULT_SHOULD_WAIT:
      fidl_end.code = fuchsia_wlan_fullmac::wire::WlanScanResult::kShouldWait;
      break;
    case WLAN_SCAN_RESULT_CANCELED_BY_DRIVER_OR_FIRMWARE:
      fidl_end.code = fuchsia_wlan_fullmac::wire::WlanScanResult::kCanceledByDriverOrFirmware;
      break;
    default:
      lerror("Unknown ScanResult code: %hhu", end->code);
      return;
  }

  auto result = ifc_client_.buffer(*arena)->OnScanEnd(fidl_end);
  if (!result.ok()) {
    lerror(
        "Failed to WlanfullmacScanEnd up in WlanFullmacIfc::OnScanEnd(). "
        "result.status: %s, txn_id=%zu\n",
        result.status_string(), end->txn_id);
    return;
  }
}

void WlanFullmacIfc::ConnectConf(const wlan_fullmac_connect_confirm_t* conf) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror(
        "Failed to create Arena in WlanFullmacIfc::ConnectConf(). "
        "status=%s\n",
        arena.status_string());
    return;
  }

  fuchsia_wlan_fullmac::wire::WlanFullmacConnectConfirm fidl_conf;

  memcpy(fidl_conf.peer_sta_address.data(), conf->peer_sta_address,
         fuchsia_wlan_ieee80211::wire::kMacAddrLen);

  // Invalid StatusCode value will be caught by FIDL transport.
  fidl_conf.result_code = static_cast<fuchsia_wlan_ieee80211::wire::StatusCode>(conf->result_code);

  fidl_conf.association_id = conf->association_id;
  fidl_conf.association_ies = fidl::VectorView<uint8_t>::FromExternal(
      (uint8_t*)conf->association_ies_list, conf->association_ies_count);

  auto result = ifc_client_.buffer(*arena)->ConnectConf(fidl_conf);
  if (!result.ok()) {
    lerror(
        "Failed to WlanFullmacConnectConfirm up in WlanFullmacIfc::ConnectConf(). "
        "result.status: %s, association_id=%zu\n",
        result.status_string(), conf->association_id);
  }
}

void WlanFullmacIfc::RoamConf(const wlan_fullmac_roam_confirm_t* conf) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror(
        "Failed to create Arena in WlanFullmacIfc::RoamConf(). "
        "status=%s\n",
        arena.status_string());
    return;
  }

  fuchsia_wlan_fullmac::wire::WlanFullmacRoamConfirm fidl_conf;
  memcpy(fidl_conf.target_bssid.data(), conf->target_bssid,
         fuchsia_wlan_ieee80211::wire::kMacAddrLen);

  // Invalid StatusCode will be caught by FIDL transport.
  fidl_conf.result_code = static_cast<fuchsia_wlan_ieee80211::wire::StatusCode>(conf->result_code);

  if (fidl_conf.result_code == fuchsia_wlan_ieee80211::wire::StatusCode::kSuccess) {
    ConvertBssDescription(conf->selected_bss, &fidl_conf.selected_bss, *arena);
  }

  auto result = ifc_client_.buffer(*arena)->RoamConf(fidl_conf);
  if (!result.ok()) {
    lerror(
        "Failed to WlanFullmacRoamConf up in WlanFullmacIfc::RoamConf(). "
        "result.status: %s\n",
        result.status_string());
  }
}

void WlanFullmacIfc::AuthInd(const wlan_fullmac_auth_ind_t* ind) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror(
        "Failed to create Arena in WlanFullmacIfc::AuthInd(). "
        "status=%s\n",
        arena.status_string());
    return;
  }

  fuchsia_wlan_fullmac::wire::WlanFullmacAuthInd fidl_ind;
  memcpy(fidl_ind.peer_sta_address.data(), ind->peer_sta_address,
         fuchsia_wlan_ieee80211::wire::kMacAddrLen);

  switch (ind->auth_type) {
    case WLAN_AUTH_TYPE_OPEN_SYSTEM:
      fidl_ind.auth_type = fuchsia_wlan_fullmac::wire::WlanAuthType::kOpenSystem;
      break;
    case WLAN_AUTH_TYPE_SHARED_KEY:
      fidl_ind.auth_type = fuchsia_wlan_fullmac::wire::WlanAuthType::kSharedKey;
      break;
    case WLAN_AUTH_TYPE_FAST_BSS_TRANSITION:
      fidl_ind.auth_type = fuchsia_wlan_fullmac::wire::WlanAuthType::kFastBssTransition;
      break;
    case WLAN_AUTH_TYPE_SAE:
      fidl_ind.auth_type = fuchsia_wlan_fullmac::wire::WlanAuthType::kSae;
      break;
    default:
      lerror("Unknown WlanAuthType: %hhu", ind->auth_type);
      return;
  }

  auto result = ifc_client_.buffer(*arena)->AuthInd(fidl_ind);
  if (!result.ok()) {
    lerror(
        "Failed to WlanFullmacAuthInd up in WlanFullmacIfc::AuthInd(). "
        "result.status: %s\n",
        result.status_string());
  }
}

void WlanFullmacIfc::DeauthConf(const wlan_fullmac_deauth_confirm_t* conf) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror(
        "Failed to create Arena in WlanFullmacIfc::DeauthConf(). "
        "status=%s\n",
        arena.status_string());
    return;
  }

  fuchsia_wlan_fullmac::wire::WlanFullmacDeauthConfirm fidl_conf;

  memcpy(fidl_conf.peer_sta_address.data(), conf->peer_sta_address,
         fuchsia_wlan_ieee80211::wire::kMacAddrLen);

  auto result = ifc_client_.buffer(*arena)->DeauthConf(fidl_conf);
  if (!result.ok()) {
    lerror(
        "Failed to WlanFullmacDeauthConfirm up in WlanFullmacIfc::DeauthConf(). "
        "result.status: %s\n",
        result.status_string());
  }
}

void WlanFullmacIfc::DeauthInd(const wlan_fullmac_deauth_indication_t* ind) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror(
        "Failed to create Arena in WlanFullmacIfc::DeauthInd(). "
        "status=%s\n",
        arena.status_string());
    return;
  }

  fuchsia_wlan_fullmac::wire::WlanFullmacDeauthIndication fidl_ind;
  memcpy(fidl_ind.peer_sta_address.data(), ind->peer_sta_address,
         fuchsia_wlan_ieee80211::wire::kMacAddrLen);
  fidl_ind.reason_code = static_cast<fuchsia_wlan_ieee80211::wire::ReasonCode>(ind->reason_code);
  fidl_ind.locally_initiated = ind->locally_initiated;

  auto result = ifc_client_.buffer(*arena)->DeauthInd(fidl_ind);
  if (!result.ok()) {
    lerror(
        "Failed to WlanFullmacDeauthIndication up in WlanFullmacIfc::DeauthInd(). "
        "result.status: %s\n",
        result.status_string());
  }
}

void WlanFullmacIfc::AssocInd(const wlan_fullmac_assoc_ind_t* ind) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror(
        "Failed to create Arena in WlanFullmacIfc::AssocInd(). "
        "status=%s\n",
        arena.status_string());
    return;
  }

  fuchsia_wlan_fullmac::wire::WlanFullmacAssocInd fidl_ind;

  memcpy(fidl_ind.peer_sta_address.data(), ind->peer_sta_address,
         fuchsia_wlan_ieee80211::wire::kMacAddrLen);
  fidl_ind.listen_interval = ind->listen_interval;
  fidl_ind.ssid.len = ind->ssid.len;
  memcpy(fidl_ind.ssid.data.data(), ind->ssid.data, ind->ssid.len);
  fidl_ind.rsne_len = ind->rsne_len;
  memcpy(fidl_ind.rsne.data(), ind->rsne, fuchsia_wlan_ieee80211::wire::kWlanIeBodyMaxLen);
  fidl_ind.vendor_ie_len = ind->vendor_ie_len;
  memcpy(fidl_ind.vendor_ie.data(), ind->vendor_ie, fuchsia_wlan_fullmac::wire::kWlanVieMaxLen);

  auto result = ifc_client_.buffer(*arena)->AssocInd(fidl_ind);
  if (!result.ok()) {
    lerror(
        "Failed to WlanFullmacAssocInd up in WlanFullmacIfc::AssocInd(). "
        "result.status: %s\n",
        result.status_string());
  }
}

void WlanFullmacIfc::DisassocConf(const wlan_fullmac_disassoc_confirm_t* conf) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror(
        "Failed to create Arena in WlanFullmacIfc::DisassocConf(). "
        "status=%s\n",
        arena.status_string());
    return;
  }

  fuchsia_wlan_fullmac::wire::WlanFullmacDisassocConfirm fidl_conf;
  fidl_conf.status = conf->status;

  auto result = ifc_client_.buffer(*arena)->DisassocConf(fidl_conf);
  if (!result.ok()) {
    lerror(
        "Failed to WlanFullmacDisassocConfirm up in WlanFullmacIfc::DisassocConf(). "
        "result.status: %s\n",
        result.status_string());
  }
}

void WlanFullmacIfc::DisassocInd(const wlan_fullmac_disassoc_indication_t* ind) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror(
        "Failed to create Arena in WlanFullmacIfc::DisassocInd(). "
        "status=%s\n",
        arena.status_string());
    return;
  }

  fuchsia_wlan_fullmac::wire::WlanFullmacDisassocIndication fidl_ind;

  memcpy(fidl_ind.peer_sta_address.data(), ind->peer_sta_address,
         fuchsia_wlan_ieee80211::wire::kMacAddrLen);

  fidl_ind.reason_code = static_cast<fuchsia_wlan_ieee80211::wire::ReasonCode>(ind->reason_code);
  fidl_ind.locally_initiated = ind->locally_initiated;

  auto result = ifc_client_.buffer(*arena)->DisassocInd(fidl_ind);
  if (!result.ok()) {
    lerror(
        "Failed to WlanFullmacDisassocIndication up in WlanFullmacIfc::DisassocInd(). "
        "result.status: %s\n",
        result.status_string());
  }
}
void WlanFullmacIfc::StartConf(const wlan_fullmac_start_confirm_t* conf) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror(
        "Failed to create Arena in WlanFullmacIfc::StartConf(). "
        "status=%s\n",
        arena.status_string());
    return;
  }

  fuchsia_wlan_fullmac::wire::WlanFullmacStartConfirm fidl_conf;

  switch (conf->result_code) {
    case WLAN_START_RESULT_SUCCESS:
      fidl_conf.result_code = fuchsia_wlan_fullmac::wire::WlanStartResult::kSuccess;
      break;
    case WLAN_START_RESULT_BSS_ALREADY_STARTED_OR_JOINED:
      fidl_conf.result_code =
          fuchsia_wlan_fullmac::wire::WlanStartResult::kBssAlreadyStartedOrJoined;
      break;
    case WLAN_START_RESULT_RESET_REQUIRED_BEFORE_START:
      fidl_conf.result_code =
          fuchsia_wlan_fullmac::wire::WlanStartResult::kResetRequiredBeforeStart;
      break;
    case WLAN_START_RESULT_NOT_SUPPORTED:
      fidl_conf.result_code = fuchsia_wlan_fullmac::wire::WlanStartResult::kNotSupported;
      break;
    default:
      lerror("Unknown WlanStartResult: %hhu", conf->result_code);
      return;
  }

  auto result = ifc_client_.buffer(*arena)->StartConf(fidl_conf);
  if (!result.ok()) {
    lerror(
        "Failed to WlanFullmacStartConfirm up in WlanFullmacIfc::StartConf(). "
        "result.status: %s\n",
        result.status_string());
  }
}

void WlanFullmacIfc::StopConf(const wlan_fullmac_stop_confirm_t* conf) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror(
        "Failed to create Arena in WlanFullmacIfc::StopConf(). "
        "status=%s\n",
        arena.status_string());
    return;
  }

  fuchsia_wlan_fullmac::wire::WlanFullmacStopConfirm fidl_conf;

  switch (conf->result_code) {
    case WLAN_STOP_RESULT_SUCCESS:
      fidl_conf.result_code = fuchsia_wlan_fullmac::wire::WlanStopResult::kSuccess;
      break;
    case WLAN_STOP_RESULT_BSS_ALREADY_STOPPED:
      fidl_conf.result_code = fuchsia_wlan_fullmac::wire::WlanStopResult::kBssAlreadyStopped;
      break;
    case WLAN_STOP_RESULT_INTERNAL_ERROR:
      fidl_conf.result_code = fuchsia_wlan_fullmac::wire::WlanStopResult::kInternalError;
      break;
    default:
      lerror("Unknown WlanStopResult: %hhu", conf->result_code);
      return;
  }

  auto result = ifc_client_.buffer(*arena)->StopConf(fidl_conf);
  if (!result.ok()) {
    lerror(
        "Failed to WlanFullmacStopConfirm up in WlanFullmacIfc::StopConf(). "
        "result.status: %s\n",
        result.status_string());
  }
}

void WlanFullmacIfc::EapolConf(const wlan_fullmac_eapol_confirm_t* conf) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror(
        "Failed to create Arena in WlanFullmacIfc::EapolConf(). "
        "status=%s\n",
        arena.status_string());
    return;
  }
  fuchsia_wlan_fullmac::wire::WlanFullmacEapolConfirm fidl_conf;

  switch (conf->result_code) {
    case WLAN_EAPOL_RESULT_SUCCESS:
      fidl_conf.result_code = fuchsia_wlan_fullmac::wire::WlanEapolResult::kSuccess;
      break;
    case WLAN_EAPOL_RESULT_TRANSMISSION_FAILURE:
      fidl_conf.result_code = fuchsia_wlan_fullmac::wire::WlanEapolResult::kTransmissionFailure;
      break;
    default:
      lerror("Unknown WlanEapolResult: %hhu", conf->result_code);
  }

  memcpy(fidl_conf.dst_addr.data(), conf->dst_addr, fuchsia_wlan_ieee80211::wire::kMacAddrLen);
  auto result = ifc_client_.buffer(*arena)->EapolConf(fidl_conf);
  if (!result.ok()) {
    lerror(
        "Failed to WlanFullmacEapolConfirm up in WlanFullmacIfc::EapolConf(). "
        "result.status: %s\n",
        result.status_string());
  }
}

void WlanFullmacIfc::OnChannelSwitch(const wlan_fullmac_channel_switch_info_t* info) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror(
        "Failed to create Arena in WlanFullmacIfc::OnChannelSwitch(). "
        "status=%s\n",
        arena.status_string());
    return;
  }
  fuchsia_wlan_fullmac::wire::WlanFullmacChannelSwitchInfo fidl_info;
  fidl_info.new_channel = info->new_channel;
  auto result = ifc_client_.buffer(*arena)->OnChannelSwitch(fidl_info);
  if (!result.ok()) {
    lerror(
        "Failed to WlanFullmacChannelSwitchInfo up in WlanFullmacIfc::OnChannelSwitch(). "
        "result.status: %s\n",
        result.status_string());
  }
}

void WlanFullmacIfc::SignalReport(const wlan_fullmac_signal_report_indication* ind) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror(
        "Failed to create Arena in WlanFullmacIfc::SignalReport(). "
        "status=%s\n",
        arena.status_string());
    return;
  }

  fuchsia_wlan_fullmac::wire::WlanFullmacSignalReportIndication fidl_ind;
  fidl_ind.rssi_dbm = ind->rssi_dbm;
  fidl_ind.snr_db = ind->snr_db;

  auto result = ifc_client_.buffer(*arena)->SignalReport(fidl_ind);
  if (!result.ok()) {
    lerror(
        "Failed to WlanFullmacSignalReportIndication up in WlanFullmacIfc::SignalReport(). "
        "result.status: %s\n",
        result.status_string());
  }
}

void WlanFullmacIfc::EapolInd(const wlan_fullmac_eapol_indication_t* ind) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror(
        "Failed to create Arena in WlanFullmacIfc::EapolInd(). "
        "status=%s\n",
        arena.status_string());
    return;
  }

  fuchsia_wlan_fullmac::wire::WlanFullmacEapolIndication fidl_ind;

  memcpy(fidl_ind.src_addr.data(), ind->src_addr, fuchsia_wlan_ieee80211::wire::kMacAddrLen);
  memcpy(fidl_ind.dst_addr.data(), ind->dst_addr, fuchsia_wlan_ieee80211::wire::kMacAddrLen);
  fidl_ind.data =
      fidl::VectorView<uint8_t>::FromExternal((uint8_t*)ind->data_list, ind->data_count);

  auto result = ifc_client_.buffer(*arena)->EapolInd(fidl_ind);
  if (!result.ok()) {
    lerror(
        "Failed to WlanFullmacEapolIndication up in WlanFullmacIfc::EapolInd(). "
        "result.status: %s\n",
        result.status_string());
  }
}

void WlanFullmacIfc::RelayCapturedFrame(const wlan_fullmac_captured_frame_result_t* res) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror(
        "Failed to create Arena in WlanFullmacIfc::RelayCapturedFrame(). "
        "status=%s\n",
        arena.status_string());
    return;
  }

  fuchsia_wlan_fullmac::wire::WlanFullmacCapturedFrameResult fidl_res;

  fidl_res.data =
      fidl::VectorView<uint8_t>::FromExternal((uint8_t*)res->data_list, res->data_count);

  auto result = ifc_client_.buffer(*arena)->RelayCapturedFrame(fidl_res);
  if (!result.ok()) {
    lerror(
        "Failed to WlanFullmacCapturedFrameResult up in WlanFullmacIfc::RelayCapturedFrame(). "
        "result.status: %s\n",
        result.status_string());
  }
}
void WlanFullmacIfc::OnPmkAvailable(const wlan_fullmac_pmk_info_t* info) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror(
        "Failed to create Arena in WlanFullmacIfc::OnPmkAvailable(). "
        "status=%s\n",
        arena.status_string());
    return;
  }

  fuchsia_wlan_fullmac::wire::WlanFullmacPmkInfo fidl_info;
  fidl_info.pmk =
      fidl::VectorView<uint8_t>::FromExternal((uint8_t*)info->pmk_list, info->pmk_count);
  fidl_info.pmkid =
      fidl::VectorView<uint8_t>::FromExternal((uint8_t*)info->pmkid_list, info->pmkid_count);

  auto result = ifc_client_.buffer(*arena)->OnPmkAvailable(fidl_info);
  if (!result.ok()) {
    lerror(
        "Failed to WlanFullmacPmkInfo up in WlanFullmacIfc::OnPmkAvailable(). "
        "result.status: %s\n",
        result.status_string());
  }
}
void WlanFullmacIfc::SaeHandshakeInd(const wlan_fullmac_sae_handshake_ind_t* ind) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror(
        "Failed to create Arena in WlanFullmacIfc::SaeHandshakeInd(). "
        "status=%s\n",
        arena.status_string());
    return;
  }

  fuchsia_wlan_fullmac::wire::WlanFullmacSaeHandshakeInd fidl_ind;

  memcpy(fidl_ind.peer_sta_address.data(), ind->peer_sta_address,
         fuchsia_wlan_ieee80211::wire::kMacAddrLen);

  auto result = ifc_client_.buffer(*arena)->SaeHandshakeInd(fidl_ind);
  if (!result.ok()) {
    lerror(
        "Failed to WlanFullmacSaeHandshakeInd up in WlanFullmacIfc::SaeHandshakeInd(). "
        "result.status: %s\n",
        result.status_string());
  }
}

void WlanFullmacIfc::SaeFrameRx(const wlan_fullmac_sae_frame_t* frame) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror(
        "Failed to create Arena in WlanFullmacIfc::SaeFrameRx(). "
        "status=%s\n",
        arena.status_string());
    return;
  }

  fuchsia_wlan_fullmac::wire::WlanFullmacSaeFrame fidl_frame;

  memcpy(fidl_frame.peer_sta_address.data(), frame->peer_sta_address,
         fuchsia_wlan_ieee80211::wire::kMacAddrLen);

  // Invalid StatusCode will be caught by FIDL transport.
  fidl_frame.status_code =
      static_cast<fuchsia_wlan_ieee80211::wire::StatusCode>(frame->status_code);

  fidl_frame.seq_num = frame->seq_num;
  fidl_frame.sae_fields = fidl::VectorView<uint8_t>::FromExternal((uint8_t*)frame->sae_fields_list,
                                                                  frame->sae_fields_count);

  auto result = ifc_client_.buffer(*arena)->SaeFrameRx(fidl_frame);
  if (!result.ok()) {
    lerror(
        "Failed to WlanFullmacSaeFrame up in WlanFullmacIfc::SaeFrameRx(). "
        "result.status: %s\n",
        result.status_string());
  }
}

void ConvertWmmAcParams(const wlan_wmm_access_category_parameters_t& in,
                        fuchsia_wlan_common::wire::WlanWmmAccessCategoryParameters* out) {
  out->ecw_min = in.ecw_min;
  out->ecw_max = in.ecw_max;
  out->aifsn = in.aifsn;
  out->txop_limit = in.txop_limit;
  out->acm = in.acm;
}

void WlanFullmacIfc::OnWmmStatusResp(zx_status_t status, const wlan_wmm_parameters_t* params) {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror(
        "Failed to create Arena in WlanFullmacIfc::OnWmmStatusResp(). "
        "status=%s\n",
        arena.status_string());
    return;
  }

  fuchsia_wlan_common::wire::WlanWmmParameters fidl_params;

  fidl_params.apsd = params->apsd;
  ConvertWmmAcParams(params->ac_be_params, &fidl_params.ac_be_params);
  ConvertWmmAcParams(params->ac_bk_params, &fidl_params.ac_bk_params);
  ConvertWmmAcParams(params->ac_vi_params, &fidl_params.ac_vi_params);
  ConvertWmmAcParams(params->ac_vo_params, &fidl_params.ac_vo_params);

  auto result = ifc_client_.buffer(*arena)->OnWmmStatusResp(status, fidl_params);
  if (!result.ok()) {
    lerror(
        "Failed to WlanWmmParameters up in WlanFullmacIfc::OnWmmStatusResp(). "
        "result.status: %s\n",
        result.status_string());
  }
}

}  // namespace wlan
