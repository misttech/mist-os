// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "convert.h"

#include <fuchsia/wlan/common/c/banjo.h>
#include <fuchsia/wlan/ieee80211/cpp/fidl.h>
#include <fuchsia/wlan/internal/c/banjo.h>
#include <fuchsia/wlan/internal/cpp/fidl.h>
#include <fuchsia/wlan/stats/cpp/fidl.h>
#include <zircon/status.h>

#include <algorithm>
#include <bitset>
#include <memory>

#include <wlan/common/band.h>
#include <wlan/common/ieee80211_codes.h>
#include <wlan/drivers/log.h>

#include "debug.h"

namespace wlanif {

// This function is only guaranteed to produce a valid fuchsia.wlan.ieee80211/CSsid,
// i.e., an SSID with a valid byte length, if the input argument came from a
// fuchsia.wlan.ieee80211/Ssid.
void ConvertCSsid(const cssid_t& cssid, fuchsia_wlan_ieee80211::wire::CSsid* out_cssid) {
  // fuchsia.wlan.ieee80211/Ssid is guaranteed to have no more than 32 bytes,
  // so its size will fit in a uint8_t.
  out_cssid->len = cssid.len;
  memcpy(out_cssid->data.data(), cssid.data, out_cssid->len);
}

fuchsia_wlan_fullmac::WlanScanType ConvertScanType(wlan_scan_type_t scan_type) {
  switch (scan_type) {
    case WLAN_SCAN_TYPE_ACTIVE:
      return fuchsia_wlan_fullmac::WlanScanType::kActive;
    case WLAN_SCAN_TYPE_PASSIVE:
      return fuchsia_wlan_fullmac::WlanScanType::kPassive;
    default:
      ZX_PANIC("Unknown scan type: %u\n", scan_type);
  }
}

void ConvertScanReq(const wlan_fullmac_impl_start_scan_request_t& in,
                    fuchsia_wlan_fullmac::wire::WlanFullmacImplStartScanRequest* out,
                    fidl::AnyArena& arena) {
  auto builder = fuchsia_wlan_fullmac::wire::WlanFullmacImplStartScanRequest::Builder(arena);
  builder.txn_id(in.txn_id);
  builder.scan_type(ConvertScanType(in.scan_type));
  auto channels_vec = std::vector<uint8_t>(in.channels_list, in.channels_list + in.channels_count);
  builder.channels(fidl::VectorView<uint8_t>(arena, channels_vec));

  std::vector<fuchsia_wlan_ieee80211::wire::CSsid> ssids_data;

  for (size_t i = 0; i < in.ssids_count; i++) {
    fuchsia_wlan_ieee80211::wire::CSsid cssid;
    cssid.len = in.ssids_list[i].len;
    memcpy(cssid.data.begin(), in.ssids_list[i].data, in.ssids_list[i].len);
    ssids_data.push_back(cssid);
  }

  builder.ssids(fidl::VectorView<fuchsia_wlan_ieee80211::wire::CSsid>(arena, ssids_data));
  builder.min_channel_time(in.min_channel_time);
  builder.max_channel_time(in.max_channel_time);
  *out = builder.Build();
}

fuchsia_wlan_common::wire::BssType ConvertBssType(const bss_type_t& in) {
  switch (in) {
    case BSS_TYPE_UNKNOWN:
      return fuchsia_wlan_common::wire::BssType::kUnknown;
    case BSS_TYPE_INFRASTRUCTURE:
      return fuchsia_wlan_common::wire::BssType::kInfrastructure;
    case BSS_TYPE_INDEPENDENT:
      return fuchsia_wlan_common::wire::BssType::kIndependent;
    case BSS_TYPE_MESH:
      return fuchsia_wlan_common::wire::BssType::kMesh;
    case BSS_TYPE_PERSONAL:
      return fuchsia_wlan_common::wire::BssType::kPersonal;
    default:
      ZX_PANIC("Invalid bss type: %u", in);
  }
}

fuchsia_wlan_common::wire::ChannelBandwidth ConvertChannelBandwidth(const channel_bandwidth_t& in) {
  switch (in) {
    case CHANNEL_BANDWIDTH_CBW20:
      return fuchsia_wlan_common::wire::ChannelBandwidth::kCbw20;
    case CHANNEL_BANDWIDTH_CBW40:
      return fuchsia_wlan_common::wire::ChannelBandwidth::kCbw40;
    case CHANNEL_BANDWIDTH_CBW40BELOW:
      return fuchsia_wlan_common::wire::ChannelBandwidth::kCbw40Below;
    case CHANNEL_BANDWIDTH_CBW80:
      return fuchsia_wlan_common::wire::ChannelBandwidth::kCbw80;
    case CHANNEL_BANDWIDTH_CBW160:
      return fuchsia_wlan_common::wire::ChannelBandwidth::kCbw160;
    case CHANNEL_BANDWIDTH_CBW80P80:
      return fuchsia_wlan_common::wire::ChannelBandwidth::kCbw80P80;
    default:
      ZX_PANIC("ChannelBandwidth is not supported: %u", in);
  }
}

void ConvertChannel(const wlan_channel_t& in, fuchsia_wlan_common::wire::WlanChannel* out) {
  out->primary = in.primary;
  out->secondary80 = in.secondary80;
  out->cbw = ConvertChannelBandwidth(in.cbw);
}

void ConvertBssDecription(const bss_description_t& in,
                          fuchsia_wlan_internal::wire::BssDescription* out, fidl::AnyArena& arena) {
  memcpy(out->bssid.data(), in.bssid, out->bssid.size());
  out->bss_type = ConvertBssType(in.bss_type);
  out->beacon_period = in.beacon_period;
  out->capability_info = in.capability_info;
  auto ies = std::vector<uint8_t>(in.ies_list, in.ies_list + in.ies_count);
  out->ies = fidl::VectorView<uint8_t>(arena, ies);
  ConvertChannel(in.channel, &out->channel);
  out->rssi_dbm = in.rssi_dbm;
  out->snr_db = in.snr_db;
}

fuchsia_wlan_fullmac::wire::WlanAuthType ConvertWlanAuthType(const wlan_auth_type_t& in) {
  switch (in) {
    case WLAN_AUTH_TYPE_OPEN_SYSTEM:
      return fuchsia_wlan_fullmac::wire::WlanAuthType::kOpenSystem;
    case WLAN_AUTH_TYPE_SHARED_KEY:
      return fuchsia_wlan_fullmac::wire::WlanAuthType::kSharedKey;
    case WLAN_AUTH_TYPE_FAST_BSS_TRANSITION:
      return fuchsia_wlan_fullmac::wire::WlanAuthType::kFastBssTransition;
    case WLAN_AUTH_TYPE_SAE:
      return fuchsia_wlan_fullmac::wire::WlanAuthType::kSae;
    default:
      ZX_PANIC("Unknown auth type: %hhu", in);
  }
}

fuchsia_wlan_common::wire::WlanKeyType ConvertWlanKeyType(const wlan_key_type_t& in) {
  switch (in) {
    case WLAN_KEY_TYPE_PAIRWISE:
      return fuchsia_wlan_common::wire::WlanKeyType::kPairwise;
    case WLAN_KEY_TYPE_GROUP:
      return fuchsia_wlan_common::wire::WlanKeyType::kGroup;
    case WLAN_KEY_TYPE_IGTK:
      return fuchsia_wlan_common::wire::WlanKeyType::kIgtk;
    case WLAN_KEY_TYPE_PEER:
      return fuchsia_wlan_common::wire::WlanKeyType::kPeer;
    case 0:
      // If the input value is 0, it means that this field has not been set from higher layer.
      // Translate it to kPairwise for now to keep FIDL happy. After all the types are defined as
      // table, this field becomes optional, so that we don't need a default value to represent the
      // uninitialized state then.
      // TODO(b/288313560): Remove this when WlanKeyType becomes table type.
      return fuchsia_wlan_common::wire::WlanKeyType::kPairwise;
    default:
      ZX_PANIC("Unknown key type: %hhu", in);
  }
}

fuchsia_wlan_ieee80211::wire::CipherSuiteType ConvertCipherSuiteType(const cipher_suite_type_t in) {
  switch (in) {
    case CIPHER_SUITE_TYPE_USE_GROUP:
      return fuchsia_wlan_ieee80211::wire::CipherSuiteType::kUseGroup;
    case CIPHER_SUITE_TYPE_WEP_40:
      return fuchsia_wlan_ieee80211::wire::CipherSuiteType::kWep40;
    case CIPHER_SUITE_TYPE_TKIP:
      return fuchsia_wlan_ieee80211::wire::CipherSuiteType::kTkip;
    case CIPHER_SUITE_TYPE_RESERVED_3:
      return fuchsia_wlan_ieee80211::wire::CipherSuiteType::kReserved3;
    case CIPHER_SUITE_TYPE_CCMP_128:
      return fuchsia_wlan_ieee80211::wire::CipherSuiteType::kCcmp128;
    case CIPHER_SUITE_TYPE_WEP_104:
      return fuchsia_wlan_ieee80211::wire::CipherSuiteType::kWep104;
    case CIPHER_SUITE_TYPE_BIP_CMAC_128:
      return fuchsia_wlan_ieee80211::wire::CipherSuiteType::kBipCmac128;
    case CIPHER_SUITE_TYPE_GROUP_ADDRESSED_NOT_ALLOWED:
      return fuchsia_wlan_ieee80211::wire::CipherSuiteType::kGroupAddressedNotAllowed;
    case CIPHER_SUITE_TYPE_GCMP_128:
      return fuchsia_wlan_ieee80211::wire::CipherSuiteType::kGcmp128;
    case CIPHER_SUITE_TYPE_GCMP_256:
      return fuchsia_wlan_ieee80211::wire::CipherSuiteType::kGcmp256;
    case CIPHER_SUITE_TYPE_CCMP_256:
      return fuchsia_wlan_ieee80211::wire::CipherSuiteType::kCcmp256;
    case CIPHER_SUITE_TYPE_BIP_GMAC_128:
      return fuchsia_wlan_ieee80211::wire::CipherSuiteType::kBipGmac128;
    case CIPHER_SUITE_TYPE_BIP_GMAC_256:
      return fuchsia_wlan_ieee80211::wire::CipherSuiteType::kBipGmac256;
    case CIPHER_SUITE_TYPE_BIP_CMAC_256:
      return fuchsia_wlan_ieee80211::wire::CipherSuiteType::kBipCmac256;
    case CIPHER_SUITE_TYPE_RESERVED_14_TO_255:
      return fuchsia_wlan_ieee80211::wire::CipherSuiteType::kReserved14To255;
    default:
      ZX_PANIC("Unknown cipher suite type: %u", in);
  }
}

void ConvertSetKeyDescriptor(const set_key_descriptor_t& in,
                             fuchsia_wlan_fullmac::wire::SetKeyDescriptor* out,
                             fidl::AnyArena& arena) {
  auto key_vec = std::vector<uint8_t>(in.key_list, in.key_list + in.key_count);
  out->key = fidl::VectorView<uint8_t>(arena, key_vec);
  out->key_id = in.key_id;
  out->key_type = ConvertWlanKeyType(in.key_type);
  memcpy(out->address.data(), in.address, out->address.size());
  out->rsc = in.rsc;
  memcpy(out->cipher_suite_oui.data(), in.cipher_suite_oui, 3);
  out->cipher_suite_type = ConvertCipherSuiteType(in.cipher_suite_type);
}

void ConvertDeleteKeyDescriptor(const delete_key_descriptor_t& in,
                                fuchsia_wlan_fullmac::wire::DeleteKeyDescriptor* out) {
  out->key_id = in.key_id;
  out->key_type = ConvertWlanKeyType(in.key_type);
  memcpy(out->address.data(), in.address, fuchsia_wlan_ieee80211::wire::kMacAddrLen);
}

void ConvertConnectReq(const wlan_fullmac_impl_connect_request_t& in,
                       fuchsia_wlan_fullmac::wire::WlanFullmacImplConnectRequest* out,
                       fidl::AnyArena& arena) {
  auto builder = fuchsia_wlan_fullmac::wire::WlanFullmacImplConnectRequest::Builder(arena);
  fuchsia_wlan_internal::wire::BssDescription bss;
  ConvertBssDecription(in.selected_bss, &bss, arena);
  builder.selected_bss(bss);
  builder.connect_failure_timeout(in.connect_failure_timeout);
  builder.auth_type(ConvertWlanAuthType(in.auth_type));
  auto sae_password =
      std::vector<uint8_t>(in.sae_password_list, in.sae_password_list + in.sae_password_count);
  builder.sae_password(fidl::VectorView<uint8_t>(arena, sae_password));
  fuchsia_wlan_fullmac::wire::SetKeyDescriptor set_key_descriptor;
  ConvertSetKeyDescriptor(in.wep_key, &set_key_descriptor, arena);
  builder.wep_key(set_key_descriptor);
  auto security_ie =
      std::vector<uint8_t>(in.security_ie_list, in.security_ie_list + in.security_ie_count);
  builder.security_ie(fidl::VectorView<uint8_t>(arena, security_ie));

  *out = builder.Build();
}

fuchsia_wlan_fullmac::wire::WlanAuthResult ConvertAuthResult(uint8_t in) {
  switch (in) {
    case WLAN_AUTH_RESULT_SUCCESS:
      return fuchsia_wlan_fullmac::wire::WlanAuthResult::kSuccess;
    case WLAN_AUTH_RESULT_REFUSED:
      return fuchsia_wlan_fullmac::wire::WlanAuthResult::kRefused;
    case WLAN_AUTH_RESULT_ANTI_CLOGGING_TOKEN_REQUIRED:
      return fuchsia_wlan_fullmac::wire::WlanAuthResult::kAntiCloggingTokenRequired;
    case WLAN_AUTH_RESULT_FINITE_CYCLIC_GROUP_NOT_SUPPORTED:
      return fuchsia_wlan_fullmac::wire::WlanAuthResult::kFiniteCyclicGroupNotSupported;
    case WLAN_AUTH_RESULT_REJECTED:
      return fuchsia_wlan_fullmac::wire::WlanAuthResult::kRejected;
    case WLAN_AUTH_RESULT_FAILURE_TIMEOUT:
      return fuchsia_wlan_fullmac::wire::WlanAuthResult::kFailureTimeout;
    default:
      ZX_PANIC("bad auth result code: %hhu\n", in);
  }
}

fuchsia_wlan_ieee80211::wire::ReasonCode ConvertReasonCode(uint16_t reason_code) {
  if (0 == reason_code) {
    return fuchsia_wlan_ieee80211::wire::ReasonCode::kReserved0;
  }
  if (67 <= reason_code && reason_code <= 127) {
    return fuchsia_wlan_ieee80211::wire::ReasonCode::kReserved67To127;
  }
  if (130 <= reason_code && reason_code <= UINT16_MAX) {
    return fuchsia_wlan_ieee80211::wire::ReasonCode::kReserved130To65535;
  }
  return static_cast<fuchsia_wlan_ieee80211::wire::ReasonCode>(reason_code);
}

fuchsia_wlan_fullmac::wire::WlanAssocResult ConvertAssocResult(uint8_t code) {
  switch (code) {
    case WLAN_ASSOC_RESULT_SUCCESS:
      return fuchsia_wlan_fullmac::wire::WlanAssocResult::kSuccess;
    case WLAN_ASSOC_RESULT_REFUSED_REASON_UNSPECIFIED:
      return fuchsia_wlan_fullmac::wire::WlanAssocResult::kRefusedReasonUnspecified;
    case WLAN_ASSOC_RESULT_REFUSED_NOT_AUTHENTICATED:
      return fuchsia_wlan_fullmac::wire::WlanAssocResult::kRefusedNotAuthenticated;
    case WLAN_ASSOC_RESULT_REFUSED_CAPABILITIES_MISMATCH:
      return fuchsia_wlan_fullmac::wire::WlanAssocResult::kRefusedCapabilitiesMismatch;
    case WLAN_ASSOC_RESULT_REFUSED_EXTERNAL_REASON:
      return fuchsia_wlan_fullmac::wire::WlanAssocResult::kRefusedExternalReason;
    case WLAN_ASSOC_RESULT_REFUSED_AP_OUT_OF_MEMORY:
      return fuchsia_wlan_fullmac::wire::WlanAssocResult::kRefusedApOutOfMemory;
    case WLAN_ASSOC_RESULT_REFUSED_BASIC_RATES_MISMATCH:
      return fuchsia_wlan_fullmac::wire::WlanAssocResult::kRefusedBasicRatesMismatch;
    case WLAN_ASSOC_RESULT_REJECTED_EMERGENCY_SERVICES_NOT_SUPPORTED:
      return fuchsia_wlan_fullmac::wire::WlanAssocResult::kRejectedEmergencyServicesNotSupported;
    case WLAN_ASSOC_RESULT_REFUSED_TEMPORARILY:
      return fuchsia_wlan_fullmac::wire::WlanAssocResult::kRefusedTemporarily;
    default:
      ZX_PANIC("bad assoc result code: %u\n", code);
  }
}

wlan_mac_role_t ConvertWlanMacRole(fuchsia_wlan_common::wire::WlanMacRole role) {
  switch (role) {
    case fuchsia_wlan_common::wire::WlanMacRole::kClient:
      return WLAN_MAC_ROLE_CLIENT;
    case fuchsia_wlan_common::wire::WlanMacRole::kAp:
      return WLAN_MAC_ROLE_AP;
    case fuchsia_wlan_common::wire::WlanMacRole::kMesh:
      return WLAN_MAC_ROLE_MESH;
    default:
      ZX_PANIC("Unknown wlan_mac_role_t: %u\n", (uint32_t)role);
  }
}

void ConvertHtCapabilities(const fuchsia_wlan_ieee80211::wire::HtCapabilities& in,
                           ht_capabilities_t* out) {
  ZX_ASSERT(sizeof(out->bytes) == in.bytes.size());
  // TODO(fxbug.dev/95240): The underlying bytes in
  // fuchsia.wlan.fullmac/HtCapabilitiesField are @packed so that copying them directly to
  // a byte array works. We may wish to change the FIDL definition in the future so this copy is
  // however more obviously correct.
  memcpy(out->bytes, in.bytes.data(), in.bytes.size());
}

void ConvertVhtCapabilities(const fuchsia_wlan_ieee80211::wire::VhtCapabilities& in,
                            vht_capabilities_t* out) {
  ZX_ASSERT(sizeof(out->bytes) == in.bytes.size());
  // TODO(fxbug.dev/95240): The underlying bytes in
  // fuchsia.wlan.fullmac/VhtCapabilitiesField are @packed so that copying them directly to
  // a byte array works. We may wish to change the definition in the future so this copy is however
  // more obviously correct.
  memcpy(out->bytes, in.bytes.data(), in.bytes.size());
}

wlan_band_t ConvertWlanBand(fuchsia_wlan_common::wire::WlanBand in) {
  switch (in) {
    case fuchsia_wlan_common::wire::WlanBand::kTwoGhz:
      return WLAN_BAND_TWO_GHZ;
    case fuchsia_wlan_common::wire::WlanBand::kFiveGhz:
      return WLAN_BAND_FIVE_GHZ;
    default:
      ZX_PANIC("Unknown wlan band: %hhu", (uint8_t)in);
  }
}

void ConvertBandCapability(const fuchsia_wlan_fullmac::wire::WlanFullmacBandCapability& in,
                           wlan_fullmac_band_capability_t* out) {
  out->band = ConvertWlanBand(in.band);

  // basic_rates
  out->basic_rate_count = in.basic_rate_count;
  memcpy(out->basic_rate_list, in.basic_rate_list.data(), out->basic_rate_count);

  // channels
  out->operating_channel_count = in.operating_channel_count;
  memcpy(out->operating_channel_list, in.operating_channel_list.data(), in.operating_channel_count);

  // ht and vht capabilities
  out->ht_supported = in.ht_supported;
  ConvertHtCapabilities(in.ht_caps, &out->ht_caps);
  out->vht_supported = in.vht_supported;
  ConvertVhtCapabilities(in.vht_caps, &out->vht_caps);
}

void ConvertQueryInfo(fuchsia_wlan_fullmac::wire::WlanFullmacQueryInfo& in,
                      wlan_fullmac_query_info_t* out) {
  memcpy(out->sta_addr, in.sta_addr.data(), fuchsia_wlan_ieee80211::wire::kMacAddrLen);
  out->role = ConvertWlanMacRole(in.role);
  out->features = in.features;
  out->band_cap_count = in.band_cap_count;
  for (size_t band_idx = 0; band_idx < in.band_cap_count; band_idx++) {
    ConvertBandCapability(in.band_cap_list.data()[band_idx], &out->band_cap_list[band_idx]);
  }
}

void ConvertMacSublayerSupport(const fuchsia_wlan_common::wire::MacSublayerSupport& in,
                               mac_sublayer_support_t* out) {
  out->rate_selection_offload.supported = in.rate_selection_offload.supported;
  switch (in.data_plane.data_plane_type) {
    case fuchsia_wlan_common::wire::DataPlaneType::kEthernetDevice:
      out->data_plane.data_plane_type = DATA_PLANE_TYPE_ETHERNET_DEVICE;
      break;
    case fuchsia_wlan_common::wire::DataPlaneType::kGenericNetworkDevice:
      out->data_plane.data_plane_type = DATA_PLANE_TYPE_GENERIC_NETWORK_DEVICE;
      break;
    default:
      ZX_PANIC("DataPlaneType is not supported: %hhu",
               static_cast<uint8_t>(in.data_plane.data_plane_type));
  }

  out->device.is_synthetic = in.device.is_synthetic;
  switch (in.device.mac_implementation_type) {
    case fuchsia_wlan_common::wire::MacImplementationType::kSoftmac:
      out->device.mac_implementation_type = MAC_IMPLEMENTATION_TYPE_SOFTMAC;
      break;
    case fuchsia_wlan_common::wire::MacImplementationType::kFullmac:
      out->device.mac_implementation_type = MAC_IMPLEMENTATION_TYPE_FULLMAC;
      break;
    default:
      ZX_PANIC("MacImplementationType is not supported: %hhu",
               static_cast<uint8_t>(in.device.mac_implementation_type));
  }

  out->device.tx_status_report_supported = in.device.tx_status_report_supported;
}

void ConvertSecuritySupport(const fuchsia_wlan_common::wire::SecuritySupport& in,
                            security_support_t* out) {
  out->sae.driver_handler_supported = in.sae.driver_handler_supported;
  out->sae.sme_handler_supported = in.sae.sme_handler_supported;
  out->mfp.supported = in.mfp.supported;
}

void ConvertSpectrumManagementSupport(
    const fuchsia_wlan_common::wire::SpectrumManagementSupport& in,
    spectrum_management_support_t* out) {
  out->dfs.supported = in.dfs.supported;
}

wlan_fullmac_hist_scope_t ConvertHistScope(fuchsia_wlan_fullmac::wire::WlanFullmacHistScope in) {
  switch (in) {
    case fuchsia_wlan_fullmac::wire::WlanFullmacHistScope::kStation:
      return WLAN_FULLMAC_HIST_SCOPE_STATION;
    case fuchsia_wlan_fullmac::wire::WlanFullmacHistScope::kPerAntenna:
      return WLAN_FULLMAC_HIST_SCOPE_PER_ANTENNA;
    default:
      ZX_PANIC("Unknown hist scope: %hhu", static_cast<uint8_t>(in));
  }
}

void ConvertAntennaId(fuchsia_wlan_fullmac::wire::WlanFullmacAntennaId in,
                      wlan_fullmac_antenna_id_t* out) {
  switch (in.freq) {
    case fuchsia_wlan_fullmac::wire::WlanFullmacAntennaFreq::kAntenna2G:
      out->freq = WLAN_FULLMAC_ANTENNA_FREQ_ANTENNA_2_G;
      break;
    case fuchsia_wlan_fullmac::wire::WlanFullmacAntennaFreq::kAntenna5G:
      out->freq = WLAN_FULLMAC_ANTENNA_FREQ_ANTENNA_5_G;
      break;
    default:
      ZX_PANIC("Unknown antenna freq: %hhu", static_cast<uint8_t>(in.freq));
  }
  out->index = in.index;
}

void ConvertNoiseFloorHistogram(
    const fuchsia_wlan_fullmac::wire::WlanFullmacNoiseFloorHistogram& in,
    wlan_fullmac_noise_floor_histogram_t* out) {
  out->hist_scope = ConvertHistScope(in.hist_scope);
  ConvertAntennaId(in.antenna_id, &out->antenna_id);

  for (size_t i = 0; i < in.noise_floor_samples.count(); i++) {
    const auto& bucket = in.noise_floor_samples.data()[i];
    auto out_buckets = const_cast<wlan_fullmac_hist_bucket_t*>(out->noise_floor_samples_list);

    out_buckets[i].bucket_index = bucket.bucket_index;
    out_buckets[i].num_samples = bucket.num_samples;
  }
  out->noise_floor_samples_count = in.noise_floor_samples.count();
  out->invalid_samples = in.invalid_samples;
}

void ConvertRxRateIndexHistogram(
    const fuchsia_wlan_fullmac::wire::WlanFullmacRxRateIndexHistogram& in,
    wlan_fullmac_rx_rate_index_histogram_t* out) {
  out->hist_scope = ConvertHistScope(in.hist_scope);
  ConvertAntennaId(in.antenna_id, &out->antenna_id);

  for (size_t i = 0; i < in.rx_rate_index_samples.count(); i++) {
    const auto& bucket = in.rx_rate_index_samples.data()[i];
    auto out_buckets = const_cast<wlan_fullmac_hist_bucket_t*>(out->rx_rate_index_samples_list);

    out_buckets[i].bucket_index = bucket.bucket_index;
    out_buckets[i].num_samples = bucket.num_samples;
  }
  out->rx_rate_index_samples_count = in.rx_rate_index_samples.count();
  out->invalid_samples = in.invalid_samples;
}

void ConvertRssiHistogram(const fuchsia_wlan_fullmac::wire::WlanFullmacRssiHistogram& in,
                          wlan_fullmac_rssi_histogram_t* out) {
  out->hist_scope = ConvertHistScope(in.hist_scope);
  ConvertAntennaId(in.antenna_id, &out->antenna_id);

  for (size_t i = 0; i < in.rssi_samples.count(); i++) {
    const auto& bucket = in.rssi_samples.data()[i];
    auto out_buckets = const_cast<wlan_fullmac_hist_bucket_t*>(out->rssi_samples_list);

    out_buckets[i].bucket_index = bucket.bucket_index;
    out_buckets[i].num_samples = bucket.num_samples;
  }
  out->rssi_samples_count = in.rssi_samples.count();
  out->invalid_samples = in.invalid_samples;
}

void ConvertSnrHistogram(const fuchsia_wlan_fullmac::wire::WlanFullmacSnrHistogram& in,
                         wlan_fullmac_snr_histogram_t* out) {
  out->hist_scope = ConvertHistScope(in.hist_scope);
  ConvertAntennaId(in.antenna_id, &out->antenna_id);

  for (size_t i = 0; i < in.snr_samples.count(); i++) {
    const auto& bucket = in.snr_samples.data()[i];
    auto out_buckets = const_cast<wlan_fullmac_hist_bucket_t*>(out->snr_samples_list);

    out_buckets[i].bucket_index = bucket.bucket_index;
    out_buckets[i].num_samples = bucket.num_samples;
  }
  out->snr_samples_count = in.snr_samples.count();
  out->invalid_samples = in.invalid_samples;
}

void ConvertIfaceCounterStats(const fuchsia_wlan_fullmac::wire::WlanFullmacIfaceCounterStats& in,
                              wlan_fullmac_iface_counter_stats_t* out) {
  out->rx_unicast_total = in.rx_unicast_total;
  out->rx_unicast_drop = in.rx_unicast_drop;
  out->rx_multicast = in.rx_multicast;
  out->tx_total = in.tx_total;
  out->tx_drop = in.tx_drop;
}

fuchsia_wlan_ieee80211::wire::StatusCode ConvertStatusCode(status_code_t in) {
  // Use a default for invalid uint16_t status codes from external sources.
  switch (in) {
    case STATUS_CODE_SUCCESS:
    case STATUS_CODE_REFUSED_REASON_UNSPECIFIED:
    case STATUS_CODE_TDLS_REJECTED_ALTERNATIVE_PROVIDED:
    case STATUS_CODE_TDLS_REJECTED:
    case STATUS_CODE_SECURITY_DISABLED:
    case STATUS_CODE_UNACCEPTABLE_LIFETIME:
    case STATUS_CODE_NOT_IN_SAME_BSS:
    case STATUS_CODE_REFUSED_CAPABILITIES_MISMATCH:
    case STATUS_CODE_DENIED_NO_ASSOCIATION_EXISTS:
    case STATUS_CODE_DENIED_OTHER_REASON:
    case STATUS_CODE_UNSUPPORTED_AUTH_ALGORITHM:
    case STATUS_CODE_TRANSACTION_SEQUENCE_ERROR:
    case STATUS_CODE_CHALLENGE_FAILURE:
    case STATUS_CODE_REJECTED_SEQUENCE_TIMEOUT:
    case STATUS_CODE_DENIED_NO_MORE_STAS:
    case STATUS_CODE_REFUSED_BASIC_RATES_MISMATCH:
    case STATUS_CODE_DENIED_NO_SHORT_PREAMBLE_SUPPORT:
    case STATUS_CODE_REJECTED_SPECTRUM_MANAGEMENT_REQUIRED:
    case STATUS_CODE_REJECTED_BAD_POWER_CAPABILITY:
    case STATUS_CODE_REJECTED_BAD_SUPPORTED_CHANNELS:
    case STATUS_CODE_DENIED_NO_SHORT_SLOT_TIME_SUPPORT:
    case STATUS_CODE_DENIED_NO_HT_SUPPORT:
    case STATUS_CODE_R0KH_UNREACHABLE:
    case STATUS_CODE_DENIED_PCO_TIME_NOT_SUPPORTED:
    case STATUS_CODE_REFUSED_TEMPORARILY:
    case STATUS_CODE_ROBUST_MANAGEMENT_POLICY_VIOLATION:
    case STATUS_CODE_UNSPECIFIED_QOS_FAILURE:
    case STATUS_CODE_DENIED_INSUFFICIENT_BANDWIDTH:
    case STATUS_CODE_DENIED_POOR_CHANNEL_CONDITIONS:
    case STATUS_CODE_DENIED_QOS_NOT_SUPPORTED:
    case STATUS_CODE_REQUEST_DECLINED:
    case STATUS_CODE_INVALID_PARAMETERS:
    case STATUS_CODE_REJECTED_WITH_SUGGESTED_CHANGES:
    case STATUS_CODE_STATUS_INVALID_ELEMENT:
    case STATUS_CODE_STATUS_INVALID_GROUP_CIPHER:
    case STATUS_CODE_STATUS_INVALID_PAIRWISE_CIPHER:
    case STATUS_CODE_STATUS_INVALID_AKMP:
    case STATUS_CODE_UNSUPPORTED_RSNE_VERSION:
    case STATUS_CODE_INVALID_RSNE_CAPABILITIES:
    case STATUS_CODE_STATUS_CIPHER_OUT_OF_POLICY:
    case STATUS_CODE_REJECTED_FOR_DELAY_PERIOD:
    case STATUS_CODE_DLS_NOT_ALLOWED:
    case STATUS_CODE_NOT_PRESENT:
    case STATUS_CODE_NOT_QOS_STA:
    case STATUS_CODE_DENIED_LISTEN_INTERVAL_TOO_LARGE:
    case STATUS_CODE_STATUS_INVALID_FT_ACTION_FRAME_COUNT:
    case STATUS_CODE_STATUS_INVALID_PMKID:
    case STATUS_CODE_STATUS_INVALID_MDE:
    case STATUS_CODE_STATUS_INVALID_FTE:
    case STATUS_CODE_REQUESTED_TCLAS_NOT_SUPPORTED_BY_AP:
    case STATUS_CODE_INSUFFICIENT_TCLAS_PROCESSING_RESOURCES:
    case STATUS_CODE_TRY_ANOTHER_BSS:
    case STATUS_CODE_GAS_ADVERTISEMENT_PROTOCOL_NOT_SUPPORTED:
    case STATUS_CODE_NO_OUTSTANDING_GAS_REQUEST:
    case STATUS_CODE_GAS_RESPONSE_NOT_RECEIVED_FROM_SERVER:
    case STATUS_CODE_GAS_QUERY_TIMEOUT:
    case STATUS_CODE_GAS_QUERY_RESPONSE_TOO_LARGE:
    case STATUS_CODE_REJECTED_HOME_WITH_SUGGESTED_CHANGES:
    case STATUS_CODE_SERVER_UNREACHABLE:
    case STATUS_CODE_REJECTED_FOR_SSP_PERMISSIONS:
    case STATUS_CODE_REFUSED_UNAUTHENTICATED_ACCESS_NOT_SUPPORTED:
    case STATUS_CODE_INVALID_RSNE:
    case STATUS_CODE_U_APSD_COEXISTANCE_NOT_SUPPORTED:
    case STATUS_CODE_U_APSD_COEX_MODE_NOT_SUPPORTED:
    case STATUS_CODE_BAD_INTERVAL_WITH_U_APSD_COEX:
    case STATUS_CODE_ANTI_CLOGGING_TOKEN_REQUIRED:
    case STATUS_CODE_UNSUPPORTED_FINITE_CYCLIC_GROUP:
    case STATUS_CODE_CANNOT_FIND_ALTERNATIVE_TBTT:
    case STATUS_CODE_TRANSMISSION_FAILURE:
    case STATUS_CODE_REQUESTED_TCLAS_NOT_SUPPORTED:
    case STATUS_CODE_TCLAS_RESOURCES_EXHAUSTED:
    case STATUS_CODE_REJECTED_WITH_SUGGESTED_BSS_TRANSITION:
    case STATUS_CODE_REJECT_WITH_SCHEDULE:
    case STATUS_CODE_REJECT_NO_WAKEUP_SPECIFIED:
    case STATUS_CODE_SUCCESS_POWER_SAVE_MODE:
    case STATUS_CODE_PENDING_ADMITTING_FST_SESSION:
    case STATUS_CODE_PERFORMING_FST_NOW:
    case STATUS_CODE_PENDING_GAP_IN_BA_WINDOW:
    case STATUS_CODE_REJECT_U_PID_SETTING:
    case STATUS_CODE_REFUSED_EXTERNAL_REASON:
    case STATUS_CODE_REFUSED_AP_OUT_OF_MEMORY:
    case STATUS_CODE_REJECTED_EMERGENCY_SERVICES_NOT_SUPPORTED:
    case STATUS_CODE_QUERY_RESPONSE_OUTSTANDING:
    case STATUS_CODE_REJECT_DSE_BAND:
    case STATUS_CODE_TCLAS_PROCESSING_TERMINATED:
    case STATUS_CODE_TS_SCHEDULE_CONFLICT:
    case STATUS_CODE_DENIED_WITH_SUGGESTED_BAND_AND_CHANNEL:
    case STATUS_CODE_MCCAOP_RESERVATION_CONFLICT:
    case STATUS_CODE_MAF_LIMIT_EXCEEDED:
    case STATUS_CODE_MCCA_TRACK_LIMIT_EXCEEDED:
    case STATUS_CODE_DENIED_DUE_TO_SPECTRUM_MANAGEMENT:
    case STATUS_CODE_DENIED_VHT_NOT_SUPPORTED:
    case STATUS_CODE_ENABLEMENT_DENIED:
    case STATUS_CODE_RESTRICTION_FROM_AUTHORIZED_GDB:
    case STATUS_CODE_AUTHORIZATION_DEENABLED:
      return static_cast<fuchsia_wlan_ieee80211::wire::StatusCode>(in);
    default:
      return fuchsia_wlan_ieee80211::wire::StatusCode::kRefusedReasonUnspecified;
  }
}

void ConvertSaeHandshakeResp(const wlan_fullmac_sae_handshake_resp_t& in,
                             fuchsia_wlan_fullmac::wire::WlanFullmacSaeHandshakeResp* out) {
  memcpy(out->peer_sta_address.data(), in.peer_sta_address,
         fuchsia_wlan_ieee80211::wire::kMacAddrLen);
  out->status_code = ConvertStatusCode(in.status_code);
}

void ConvertSaeFrame(const wlan_fullmac_sae_frame_t& in,
                     fuchsia_wlan_fullmac::wire::WlanFullmacSaeFrame* out, fidl::AnyArena& arena) {
  memcpy(out->peer_sta_address.data(), in.peer_sta_address,
         fuchsia_wlan_ieee80211::wire::kMacAddrLen);
  out->status_code = ConvertStatusCode(in.status_code);
  out->seq_num = in.seq_num;
  auto sae_fields =
      std::vector<uint8_t>(in.sae_fields_list, in.sae_fields_list + in.sae_fields_count);
  out->sae_fields = fidl::VectorView<uint8_t>(arena, sae_fields);
}

bss_type_t ConvertBssType(fuchsia_wlan_common::wire::BssType in) {
  switch (in) {
    case fuchsia_wlan_common::wire::BssType::kUnknown:
      return BSS_TYPE_UNKNOWN;
    case fuchsia_wlan_common::wire::BssType::kInfrastructure:
      return BSS_TYPE_INFRASTRUCTURE;
    case fuchsia_wlan_common::wire::BssType::kIndependent:
      return BSS_TYPE_INDEPENDENT;
    case fuchsia_wlan_common::wire::BssType::kMesh:
      return BSS_TYPE_MESH;
    case fuchsia_wlan_common::wire::BssType::kPersonal:
      return BSS_TYPE_PERSONAL;
    default:
      ZX_PANIC("Invalid bss type: %u", (uint32_t)in);
  }
}

channel_bandwidth_t ConvertChannelBandwidth(const fuchsia_wlan_common::wire::ChannelBandwidth in) {
  switch (in) {
    case fuchsia_wlan_common::wire::ChannelBandwidth::kCbw20:
      return CHANNEL_BANDWIDTH_CBW20;
    case fuchsia_wlan_common::wire::ChannelBandwidth::kCbw40:
      return CHANNEL_BANDWIDTH_CBW40;
    case fuchsia_wlan_common::wire::ChannelBandwidth::kCbw40Below:
      return CHANNEL_BANDWIDTH_CBW40BELOW;
    case fuchsia_wlan_common::wire::ChannelBandwidth::kCbw80:
      return CHANNEL_BANDWIDTH_CBW80;
    case fuchsia_wlan_common::wire::ChannelBandwidth::kCbw160:
      return CHANNEL_BANDWIDTH_CBW160;
    case fuchsia_wlan_common::wire::ChannelBandwidth::kCbw80P80:
      return CHANNEL_BANDWIDTH_CBW80P80;
    default:
      ZX_PANIC("ChannelBandwidth is not supported: %u", (uint32_t)in);
  }
}

void ConvertChannel(const fuchsia_wlan_common::wire::WlanChannel& in, wlan_channel_t* out) {
  out->primary = in.primary;
  out->secondary80 = in.secondary80;
  out->cbw = ConvertChannelBandwidth(in.cbw);
}

// Not a deep copy
void ConvertBssDescription(const fuchsia_wlan_internal::wire::BssDescription& in,
                           bss_description_t* out) {
  memcpy(out->bssid, in.bssid.data(), fuchsia_wlan_ieee80211::wire::kMacAddrLen);
  out->bss_type = ConvertBssType(in.bss_type);
  out->beacon_period = in.beacon_period;
  out->capability_info = in.capability_info;
  out->ies_list = in.ies.data();
  out->ies_count = in.ies.count();
  ConvertChannel(in.channel, &out->channel);
  out->rssi_dbm = in.rssi_dbm;
  out->snr_db = in.snr_db;
}

void ConvertFullmacScanResult(const fuchsia_wlan_fullmac::wire::WlanFullmacScanResult& in,
                              wlan_fullmac_scan_result_t* out) {
  out->txn_id = in.txn_id;
  out->timestamp_nanos = in.timestamp_nanos;
  ConvertBssDescription(in.bss, &out->bss);
}

wlan_scan_result_t ConvertScanResult(fuchsia_wlan_fullmac::wire::WlanScanResult in) {
  switch (in) {
    case fuchsia_wlan_fullmac::wire::WlanScanResult::kSuccess:
      return WLAN_SCAN_RESULT_SUCCESS;
    case fuchsia_wlan_fullmac::wire::WlanScanResult::kNotSupported:
      return WLAN_SCAN_RESULT_NOT_SUPPORTED;
    case fuchsia_wlan_fullmac::wire::WlanScanResult::kInvalidArgs:
      return WLAN_SCAN_RESULT_INVALID_ARGS;
    case fuchsia_wlan_fullmac::wire::WlanScanResult::kInternalError:
      return WLAN_SCAN_RESULT_INTERNAL_ERROR;
    case fuchsia_wlan_fullmac::wire::WlanScanResult::kShouldWait:
      return WLAN_SCAN_RESULT_SHOULD_WAIT;
    case fuchsia_wlan_fullmac::wire::WlanScanResult::kCanceledByDriverOrFirmware:
      return WLAN_SCAN_RESULT_CANCELED_BY_DRIVER_OR_FIRMWARE;
    default:
      ZX_PANIC("Unknown WlanScanResult: %hhu", static_cast<uint8_t>(in));
  }
}

void ConvertScanEnd(const fuchsia_wlan_fullmac::wire::WlanFullmacScanEnd& in,
                    wlan_fullmac_scan_end_t* out) {
  out->txn_id = in.txn_id;
  out->code = ConvertScanResult(in.code);
}

status_code_t ConvertStatusCode(fuchsia_wlan_ieee80211::wire::StatusCode in) {
  // FIDL will catch invalud status code.
  return static_cast<uint16_t>(in);
}

void ConvertConnectConfirm(const fuchsia_wlan_fullmac::wire::WlanFullmacConnectConfirm& in,
                           wlan_fullmac_connect_confirm_t* out) {
  memcpy(out->peer_sta_address, in.peer_sta_address.data(),
         fuchsia_wlan_ieee80211::wire::kMacAddrLen);
  out->result_code = ConvertStatusCode(in.result_code);
  out->association_id = in.association_id;
  out->association_ies_list = in.association_ies.data();
  out->association_ies_count = in.association_ies.count();
}

void ConvertRoamConfirm(const fuchsia_wlan_fullmac::wire::WlanFullmacRoamConfirm& in,
                        wlan_fullmac_roam_confirm_t* out) {
  memcpy(out->target_bssid, in.target_bssid.data(), fuchsia_wlan_ieee80211::wire::kMacAddrLen);
  out->result_code = ConvertStatusCode(in.result_code);
  ConvertBssDescription(in.selected_bss, &out->selected_bss);
}

wlan_auth_type_t ConvertWlanAuthType(const fuchsia_wlan_fullmac::wire::WlanAuthType in) {
  switch (in) {
    case fuchsia_wlan_fullmac::wire::WlanAuthType::kOpenSystem:
      return WLAN_AUTH_TYPE_OPEN_SYSTEM;
    case fuchsia_wlan_fullmac::wire::WlanAuthType::kSharedKey:
      return WLAN_AUTH_TYPE_SHARED_KEY;
    case fuchsia_wlan_fullmac::wire::WlanAuthType::kFastBssTransition:
      return WLAN_AUTH_TYPE_FAST_BSS_TRANSITION;
    case fuchsia_wlan_fullmac::wire::WlanAuthType::kSae:
      return WLAN_AUTH_TYPE_SAE;
    default:
      ZX_PANIC("Unknown auth type: %hhu", static_cast<uint8_t>(in));
  }
}

void ConvertAuthInd(const fuchsia_wlan_fullmac::wire::WlanFullmacAuthInd& in,
                    wlan_fullmac_auth_ind_t* out) {
  memcpy(out->peer_sta_address, in.peer_sta_address.data(),
         fuchsia_wlan_ieee80211::wire::kMacAddrLen);
  out->auth_type = ConvertWlanAuthType(in.auth_type);
}

uint16_t ConvertReasonCode(fuchsia_wlan_ieee80211::wire::ReasonCode reason_code) {
  uint16_t code = static_cast<uint16_t>(reason_code);

  if (0 == code) {
    return REASON_CODE_RESERVED_0;
  }
  if (67 <= code && code <= 127) {
    return REASON_CODE_RESERVED_67_TO_127;
  }
  if (130 <= code && code <= UINT16_MAX) {
    return REASON_CODE_RESERVED_130_TO_65535;
  }
  return code;
}

void ConvertDeauthInd(const fuchsia_wlan_fullmac::wire::WlanFullmacDeauthIndication& in,
                      wlan_fullmac_deauth_indication_t* out) {
  memcpy(out->peer_sta_address, in.peer_sta_address.data(),
         fuchsia_wlan_ieee80211::wire::kMacAddrLen);
  out->reason_code = ConvertReasonCode(in.reason_code);
  out->locally_initiated = in.locally_initiated;
}

void ConvertCSsid(const fuchsia_wlan_ieee80211::wire::CSsid& cssid, cssid_t* out_cssid) {
  // fuchsia.wlan.ieee80211/Ssid is guaranteed to have no more than 32 bytes,
  // so its size will fit in a uint8_t.
  out_cssid->len = cssid.len;
  memcpy(out_cssid->data, cssid.data.data(), out_cssid->len);
}

void ConvertAssocInd(const fuchsia_wlan_fullmac::wire::WlanFullmacAssocInd& in,
                     wlan_fullmac_assoc_ind_t* out) {
  memcpy(out->peer_sta_address, in.peer_sta_address.data(),
         fuchsia_wlan_ieee80211::wire::kMacAddrLen);
  out->listen_interval = in.listen_interval;
  ConvertCSsid(in.ssid, &out->ssid);
  out->rsne_len = in.rsne_len;
  memcpy(out->rsne, in.rsne.data(), fuchsia_wlan_ieee80211::wire::kWlanIeBodyMaxLen);
  out->vendor_ie_len = in.vendor_ie_len;
  memcpy(out->vendor_ie, in.vendor_ie.data(), WLAN_VIE_MAX_LEN);
}

void ConvertDisassocInd(const fuchsia_wlan_fullmac::wire::WlanFullmacDisassocIndication& in,
                        wlan_fullmac_disassoc_indication_t* out) {
  memcpy(out->peer_sta_address, in.peer_sta_address.data(),
         fuchsia_wlan_ieee80211::wire::kMacAddrLen);
  out->reason_code = ConvertReasonCode(in.reason_code);
  out->locally_initiated = in.locally_initiated;
}

uint8_t ConvertStartResultCode(fuchsia_wlan_fullmac::wire::WlanStartResult in) {
  switch (in) {
    case fuchsia_wlan_fullmac::wire::WlanStartResult::kSuccess:
      return WLAN_START_RESULT_SUCCESS;
    case fuchsia_wlan_fullmac::wire::WlanStartResult::kBssAlreadyStartedOrJoined:
      return WLAN_START_RESULT_BSS_ALREADY_STARTED_OR_JOINED;
    case fuchsia_wlan_fullmac::wire::WlanStartResult::kResetRequiredBeforeStart:
      return WLAN_START_RESULT_RESET_REQUIRED_BEFORE_START;
    case fuchsia_wlan_fullmac::wire::WlanStartResult::kNotSupported:
      return WLAN_START_RESULT_NOT_SUPPORTED;
    default:
      ZX_PANIC("Unknown start result code: %hhu", static_cast<uint8_t>(in));
  }
}

uint8_t ConvertStopResultCode(fuchsia_wlan_fullmac::wire::WlanStopResult in) {
  switch (in) {
    case fuchsia_wlan_fullmac::wire::WlanStopResult::kSuccess:
      return WLAN_STOP_RESULT_SUCCESS;
    case fuchsia_wlan_fullmac::wire::WlanStopResult::kBssAlreadyStopped:
      return WLAN_STOP_RESULT_BSS_ALREADY_STOPPED;
    case fuchsia_wlan_fullmac::wire::WlanStopResult::kInternalError:
      return WLAN_STOP_RESULT_INTERNAL_ERROR;
    default:
      ZX_PANIC("Unknown stop result code: %hhu", static_cast<uint8_t>(in));
  }
}

uint8_t ConvertEapolResultCode(fuchsia_wlan_fullmac::wire::WlanEapolResult in) {
  switch (in) {
    case fuchsia_wlan_fullmac::wire::WlanEapolResult::kSuccess:
      return WLAN_EAPOL_RESULT_SUCCESS;
    case fuchsia_wlan_fullmac::wire::WlanEapolResult::kTransmissionFailure:
      return WLAN_EAPOL_RESULT_TRANSMISSION_FAILURE;
    default:
      ZX_PANIC("Unknown EAPOL result code: %hhu", static_cast<uint8_t>(in));
  }
}

void ConvertEapolConf(const fuchsia_wlan_fullmac::wire::WlanFullmacEapolConfirm& in,
                      wlan_fullmac_eapol_confirm_t* out) {
  out->result_code = ConvertEapolResultCode(in.result_code);
  std::memcpy(out->dst_addr, in.dst_addr.data(), fuchsia_wlan_ieee80211::wire::kMacAddrLen);
}

void ConvertEapolIndication(const fuchsia_wlan_fullmac::wire::WlanFullmacEapolIndication& in,
                            wlan_fullmac_eapol_indication_t* out) {
  memcpy(out->src_addr, in.src_addr.data(), fuchsia_wlan_ieee80211::wire::kMacAddrLen);
  memcpy(out->dst_addr, in.dst_addr.data(), fuchsia_wlan_ieee80211::wire::kMacAddrLen);
  out->data_count = in.data.count();
  out->data_list = in.data.data();
}

void ConvertSaeFrame(const fuchsia_wlan_fullmac::wire::WlanFullmacSaeFrame& in,
                     wlan_fullmac_sae_frame_t* out) {
  memcpy(out->peer_sta_address, in.peer_sta_address.data(),
         fuchsia_wlan_ieee80211::wire::kMacAddrLen);
  out->status_code = ConvertStatusCode(in.status_code);
  out->seq_num = in.seq_num;
  out->sae_fields_list = in.sae_fields.data();
  out->sae_fields_count = in.sae_fields.count();
}

void ConvertWmmAcParams(const fuchsia_wlan_common::wire::WlanWmmAccessCategoryParameters& in,
                        wlan_wmm_access_category_parameters_t* out) {
  out->ecw_min = in.ecw_min;
  out->ecw_max = in.ecw_max;
  out->aifsn = in.aifsn;
  out->txop_limit = in.txop_limit;
  out->acm = in.acm;
}

void ConvertWmmParams(const fuchsia_wlan_common::wire::WlanWmmParameters& in,
                      wlan_wmm_parameters_t* out) {
  out->apsd = in.apsd;
  ConvertWmmAcParams(in.ac_be_params, &out->ac_be_params);
  ConvertWmmAcParams(in.ac_bk_params, &out->ac_bk_params);
  ConvertWmmAcParams(in.ac_vi_params, &out->ac_vi_params);
  ConvertWmmAcParams(in.ac_vo_params, &out->ac_vo_params);
}

}  // namespace wlanif
