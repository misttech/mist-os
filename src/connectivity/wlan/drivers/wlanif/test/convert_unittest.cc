// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>
#include <src/connectivity/wlan/drivers/wlanif/convert.h>
#include <wlan/drivers/log_instance.h>
#include <wlan/drivers/test/log_overrides.h>

#include "fidl/fuchsia.wlan.ieee80211/cpp/wire_types.h"

namespace wlan::drivers {

namespace wlan_fullmac = fuchsia_wlan_fullmac::wire;
namespace wlan_common = fuchsia_wlan_common::wire;
namespace wlan_ieee80211 = fuchsia_wlan_ieee80211::wire;
namespace wlan_internal = fuchsia_wlan_internal::wire;

/* Metadata which is used as input and expected output for the under-test conversion functions*/

// Fake metadata -- general
static constexpr uint8_t kFakeMacAddr[wlan_ieee80211::kMacAddrLen] = {6, 5, 4, 3, 2, 2};
static constexpr uint32_t kFakeFeature = WLAN_FULLMAC_FEATURE_SYNTH;
static constexpr uint8_t kFakeRate = 206;
static constexpr uint8_t kFakeHtCapBytes[wlan_ieee80211::kHtCapLen] = {
    3, 1, 4, 1, 5, 9, 2, 6, 5, 3, 5, 8, 9, 7, 9, 3, 2, 3, 8, 4, 6, 2, 6, 4, 3, 3};
static constexpr uint8_t kFakeVhtCapBytes[wlan_ieee80211::kVhtCapLen] = {8, 3, 2, 7, 9, 5,
                                                                         0, 2, 8, 8, 4, 1};
static constexpr uint8_t kFakeIeLen = 18;
static constexpr uint8_t kFakeIeBytes[kFakeIeLen] = {4, 5, 0, 1, 3, 0, 9, 9, 5,
                                                     1, 0, 1, 1, 1, 4, 1, 0, 1};
static constexpr uint8_t kFakeChannel = 15;
static constexpr uint8_t kFakeSsidLen = 9;
static constexpr uint8_t kFakeSsid[kFakeSsidLen] = {'w', 'h', 'a', 't', 'a', 't', 'e', 's', 't'};
static constexpr uint8_t kFakeKey[wlan_ieee80211::kMaxKeyLen] = {
    6, 9, 3, 9, 9, 3, 7, 5, 1, 0, 5, 8, 2, 0, 9, 7, 4, 9, 4, 4, 5, 9, 2, 3, 0, 7, 8, 1, 6, 4, 0, 6};
static constexpr uint8_t kFakeOui[wlan_ieee80211::kOuiLen] = {9, 7, 1};
static constexpr size_t kFakeBucketSize = 5;
static constexpr zx_time_t kFakeTime = 6859760;

static constexpr bool kPopulaterBool = true;
static constexpr uint8_t kRandomPopulaterUint8 = 118;
static constexpr uint16_t kRandomPopulaterUint16 = 53535;
static constexpr uint32_t kRandomPopulaterUint32 = 4062722468;
static constexpr uint64_t kRandomPopulaterUint64 = 1518741085930693;
static constexpr int8_t kRandomPopulaterInt8 = -95;

// Fake metadata -- FIDL
static constexpr wlan_common::WlanMacRole kFakeFidlMacRole = wlan_common::WlanMacRole::kAp;
static constexpr wlan_common::WlanBand kFakeFidlBand = wlan_common::WlanBand::kFiveGhz;
static constexpr wlan_fullmac::WlanScanType kFakeFidlScanType = wlan_fullmac::WlanScanType::kActive;
static constexpr wlan_fullmac::WlanAuthType kFakeFidlAuthType = wlan_fullmac::WlanAuthType::kSae;
static constexpr wlan_common::BssType kFakeFidlBssType = wlan_common::BssType::kMesh;
static constexpr wlan_common::WlanKeyType kFakeFidlKeyType = wlan_common::WlanKeyType::kGroup;
static constexpr wlan_common::ChannelBandwidth kFakeFidlChannelBandwidth =
    wlan_common::ChannelBandwidth::kCbw160;
static constexpr wlan_ieee80211::CipherSuiteType kFakeFidlCipherSuiteType =
    wlan_ieee80211::CipherSuiteType::kGcmp128;
static constexpr wlan_fullmac::WlanAssocResult kFakeFidlAssocResult =
    wlan_fullmac::WlanAssocResult::kRefusedBasicRatesMismatch;
static constexpr wlan_common::DataPlaneType kFakeFidlDataPlaneType =
    wlan_common::DataPlaneType::kGenericNetworkDevice;
static constexpr wlan_common::MacImplementationType kFakeFidlMacImplementationType =
    wlan_common::MacImplementationType::kFullmac;
static constexpr wlan_fullmac::WlanFullmacAntennaFreq kFakeFidlAntennaFreq =
    wlan_fullmac::WlanFullmacAntennaFreq::kAntenna5G;
static constexpr wlan_fullmac::WlanFullmacHistScope kFakeFidlHistScope =
    wlan_fullmac::WlanFullmacHistScope::kPerAntenna;
static constexpr wlan_fullmac::WlanScanResult kFakeFidlScanResult =
    wlan_fullmac::WlanScanResult::kInvalidArgs;
static constexpr wlan_ieee80211::StatusCode kFakeFidlStatusCode =
    wlan_ieee80211::StatusCode::kTdlsRejected;
static constexpr wlan_fullmac::WlanEapolResult kFakeFidlEapolResult =
    wlan_fullmac::WlanEapolResult::kTransmissionFailure;

// Fake metadata -- banjo
static constexpr uint32_t kFakeBanjoMacRole = WLAN_MAC_ROLE_AP;
static constexpr uint8_t kFakeBanjoBand = WLAN_BAND_FIVE_GHZ;
static constexpr uint8_t kFakeBanjoScanType = WLAN_SCAN_TYPE_ACTIVE;
static constexpr uint8_t kFakeBanjoAuthType = WLAN_AUTH_TYPE_SAE;
static constexpr uint32_t kFakeBanjoBssType = BSS_TYPE_MESH;
static constexpr uint8_t kFakeBanjoKeyType = WLAN_KEY_TYPE_GROUP;
static constexpr uint32_t kFakeBanjoChannelBandwidth = CHANNEL_BANDWIDTH_CBW160;
static constexpr uint32_t kFakeBanjoCipherSuiteType = CIPHER_SUITE_TYPE_GCMP_128;
static constexpr uint8_t kFakeBanjoAssocResult = WLAN_ASSOC_RESULT_REFUSED_BASIC_RATES_MISMATCH;
static constexpr uint8_t kFakeBanjoDataPlaneType = DATA_PLANE_TYPE_GENERIC_NETWORK_DEVICE;
static constexpr uint8_t kFakeBanjoMacImplementationType = MAC_IMPLEMENTATION_TYPE_FULLMAC;
static constexpr uint8_t kFakeBanjoAntennaFreq = WLAN_FULLMAC_ANTENNA_FREQ_ANTENNA_5_G;
static constexpr uint8_t kFakeBanjoHistScope = WLAN_FULLMAC_HIST_SCOPE_PER_ANTENNA;
static constexpr uint8_t kFakeBanjoScanResult = WLAN_SCAN_RESULT_INVALID_ARGS;
static constexpr uint16_t kFakeBanjoStatusCode = STATUS_CODE_TDLS_REJECTED;
static constexpr uint8_t kFakeBanjoEapolResult = WLAN_EAPOL_RESULT_TRANSMISSION_FAILURE;

class ConvertTest : public LogTest {};

/* Tests for to-FIDL conversions */

TEST_F(ConvertTest, ToFidlStartScanRequest) {
  wlan_fullmac_impl_start_scan_request_t in;
  in.txn_id = kRandomPopulaterUint64;
  in.scan_type = kFakeBanjoScanType;

  uint8_t channels_array[wlan_ieee80211::kMaxUniqueChannelNumbers];
  for (size_t i = 0; i < wlan_ieee80211::kMaxUniqueChannelNumbers; i++) {
    channels_array[i] = kFakeChannel;
  }
  in.channels_list = channels_array;
  in.channels_count = wlan_ieee80211::kMaxUniqueChannelNumbers;

  cssid_t ssids_array[wlan_ieee80211::kSsidListMax];
  for (size_t i = 0; i < wlan_ieee80211::kSsidListMax; i++) {
    ssids_array[i].len = kFakeSsidLen;
    memcpy(ssids_array[i].data, kFakeSsid, kFakeSsidLen);
  }
  in.ssids_list = ssids_array;
  in.ssids_count = wlan_ieee80211::kSsidListMax;
  in.min_channel_time = kRandomPopulaterUint32;
  in.max_channel_time = kRandomPopulaterUint32;

  fidl::Arena arena;
  fuchsia_wlan_fullmac::wire::WlanFullmacImplStartScanRequest out;
  wlanif::ConvertScanReq(in, &out, arena);

  EXPECT_EQ(kRandomPopulaterUint64, out.txn_id());
  EXPECT_EQ(kFakeFidlScanType, out.scan_type());
  EXPECT_EQ(wlan_ieee80211::kMaxUniqueChannelNumbers, out.channels().count());
  for (size_t i = 0; i < wlan_ieee80211::kMaxUniqueChannelNumbers; i++) {
    EXPECT_EQ(kFakeChannel, out.channels().data()[i]);
  }
  EXPECT_EQ(wlan_ieee80211::kSsidListMax, out.ssids().count());
  for (size_t i = 0; i < wlan_ieee80211::kSsidListMax; i++) {
    EXPECT_EQ(kFakeSsidLen, out.ssids().data()[i].len);
    EXPECT_EQ(0, memcmp(kFakeSsid, out.ssids().data()[i].data.data(), kFakeSsidLen));
  }
  EXPECT_EQ(kRandomPopulaterUint32, out.min_channel_time());
  EXPECT_EQ(kRandomPopulaterUint32, out.max_channel_time());
}

TEST_F(ConvertTest, ToFidlQueryConnectReqRequest) {
  wlan_fullmac_impl_connect_request_t in;
  bss_description_t bss;

  memcpy(bss.bssid, kFakeMacAddr, sizeof(kFakeMacAddr));
  bss.bss_type = kFakeBanjoBssType;
  bss.beacon_period = kRandomPopulaterUint16;
  bss.capability_info = kRandomPopulaterUint16;
  bss.ies_list = kFakeIeBytes;
  bss.ies_count = kFakeIeLen;
  bss.channel.primary = kFakeChannel;
  bss.channel.cbw = kFakeBanjoChannelBandwidth;
  bss.channel.secondary80 = kFakeChannel;
  bss.rssi_dbm = kRandomPopulaterInt8;
  bss.snr_db = kRandomPopulaterInt8;
  in.selected_bss = bss;

  in.connect_failure_timeout = kRandomPopulaterUint32;
  in.auth_type = kFakeBanjoAuthType;

  in.sae_password_list = kFakeKey;
  in.sae_password_count = wlan_ieee80211::kMaxKeyLen;

  set_key_descriptor_t set_key;
  set_key.key_list = kFakeKey;
  set_key.key_count = wlan_ieee80211::kMaxKeyLen;
  set_key.key_id = kRandomPopulaterUint16;
  set_key.key_type = kFakeBanjoKeyType;
  memcpy(set_key.address, kFakeMacAddr, sizeof(kFakeMacAddr));
  set_key.rsc = kRandomPopulaterUint64;
  for (size_t i = 0; i < wlan_ieee80211::kOuiLen; i++) {
    set_key.cipher_suite_oui[i] = kFakeOui[i];
  }
  set_key.cipher_suite_type = kFakeBanjoCipherSuiteType;
  in.wep_key = set_key;

  in.security_ie_list = kFakeIeBytes;
  in.security_ie_count = kFakeIeLen;

  fidl::Arena arena;
  fuchsia_wlan_fullmac::wire::WlanFullmacImplConnectRequest out;

  wlanif::ConvertConnectReq(in, &out, arena);

  EXPECT_EQ(0, memcmp(out.selected_bss().bssid.data(), kFakeMacAddr, sizeof(kFakeMacAddr)));
  EXPECT_EQ(kFakeFidlBssType, out.selected_bss().bss_type);
  EXPECT_EQ(kRandomPopulaterUint16, out.selected_bss().beacon_period);
  EXPECT_EQ(kRandomPopulaterUint16, out.selected_bss().capability_info);
  EXPECT_EQ(0, memcmp(out.selected_bss().ies.data(), kFakeIeBytes, kFakeIeLen));
  EXPECT_EQ(kFakeIeLen, out.selected_bss().ies.count());
  EXPECT_EQ(kFakeChannel, out.selected_bss().channel.primary);
  EXPECT_EQ(kFakeFidlChannelBandwidth, out.selected_bss().channel.cbw);
  EXPECT_EQ(kFakeChannel, out.selected_bss().channel.secondary80);
  EXPECT_EQ(kRandomPopulaterInt8, out.selected_bss().rssi_dbm);
  EXPECT_EQ(kRandomPopulaterInt8, out.selected_bss().snr_db);

  EXPECT_EQ(kRandomPopulaterUint32, out.connect_failure_timeout());
  EXPECT_EQ(kFakeFidlAuthType, out.auth_type());
  EXPECT_EQ(0, memcmp(out.sae_password().data(), kFakeKey, wlan_ieee80211::kMaxKeyLen));
  EXPECT_EQ(wlan_ieee80211::kMaxKeyLen, out.sae_password().count());

  EXPECT_EQ(0, memcmp(out.wep_key().key.data(), kFakeKey, wlan_ieee80211::kMaxKeyLen));
  EXPECT_EQ(wlan_ieee80211::kMaxKeyLen, out.wep_key().key.count());

  EXPECT_EQ(kRandomPopulaterUint16, out.wep_key().key_id);
  EXPECT_EQ(kFakeFidlKeyType, out.wep_key().key_type);
  EXPECT_EQ(0, memcmp(out.wep_key().address.data(), kFakeMacAddr, sizeof(kFakeMacAddr)));
  EXPECT_EQ(kRandomPopulaterUint64, out.wep_key().rsc);
  for (size_t i = 0; i < wlan_ieee80211::kOuiLen; i++) {
    EXPECT_EQ(kFakeOui[i], out.wep_key().cipher_suite_oui[i]);
  }
  EXPECT_EQ(kFakeFidlCipherSuiteType, out.wep_key().cipher_suite_type);
  EXPECT_EQ(0, memcmp(out.security_ie().data(), kFakeIeBytes, kFakeIeLen));
  EXPECT_EQ(kFakeIeLen, out.security_ie().count());
}

TEST_F(ConvertTest, ToFidlDeleteKeyDescriptor) {
  delete_key_descriptor_t in;
  in.key_id = kRandomPopulaterUint16;
  in.key_type = kFakeBanjoKeyType;
  memcpy(in.address, kFakeMacAddr, sizeof(kFakeMacAddr));

  fuchsia_wlan_fullmac::wire::DeleteKeyDescriptor out;
  wlanif::ConvertDeleteKeyDescriptor(in, &out);

  EXPECT_EQ(kRandomPopulaterUint16, out.key_id);
  EXPECT_EQ(kFakeFidlKeyType, out.key_type);
  EXPECT_EQ(0, memcmp(kFakeMacAddr, out.address.data(), sizeof(kFakeMacAddr)));
}

TEST_F(ConvertTest, ToFidlAssocResult) {
  uint8_t in = kFakeBanjoAssocResult;
  wlan_fullmac::WlanAssocResult out = wlanif::ConvertAssocResult(in);
  EXPECT_EQ(kFakeFidlAssocResult, out);
}

/* Tests for to-banjo conversions */

TEST_F(ConvertTest, ToBanjoQueryInfo) {
  log::Instance::Init(0);
  fidl::Arena arena;
  wlan_fullmac::WlanFullmacQueryInfo in;

  fidl::Array<uint8_t, wlan_ieee80211::kMacAddrLen> sta_addr;
  memcpy(sta_addr.begin(), kFakeMacAddr, sta_addr.size());
  in.sta_addr = sta_addr;
  in.role = kFakeFidlMacRole;
  in.features = kFakeFeature;

  fidl::Array<wlan_fullmac::WlanFullmacBandCapability, wlan_common::kMaxBands> band_caps_buffer;

  for (size_t i = 0; i < wlan_common::kMaxBands; i++) {
    band_caps_buffer[i].band = kFakeFidlBand;
    band_caps_buffer[i].basic_rate_count = wlan_ieee80211::kMaxSupportedBasicRates;

    fidl::Array<uint8_t, wlan_ieee80211::kMaxSupportedBasicRates> basic_rate_array;
    for (size_t j = 0; j < wlan_ieee80211::kMaxSupportedBasicRates; j++) {
      basic_rate_array[j] = kFakeRate;
    }
    band_caps_buffer[i].basic_rate_list = basic_rate_array;
    band_caps_buffer[i].ht_supported = kPopulaterBool;
    wlan_ieee80211::HtCapabilities ht_caps;
    memcpy(ht_caps.bytes.begin(), kFakeHtCapBytes, wlan_ieee80211::kHtCapLen);
    band_caps_buffer[i].ht_caps = ht_caps;
    band_caps_buffer[i].vht_supported = kPopulaterBool;
    wlan_ieee80211::VhtCapabilities vht_caps;
    memcpy(vht_caps.bytes.begin(), kFakeVhtCapBytes, wlan_ieee80211::kVhtCapLen);
    band_caps_buffer[i].vht_caps = vht_caps;
    band_caps_buffer[i].operating_channel_count = wlan_ieee80211::kMaxUniqueChannelNumbers;
    fidl::Array<uint8_t, wlan_ieee80211::kMaxUniqueChannelNumbers> operating_channel_array;

    for (size_t j = 0; j < wlan_ieee80211::kMaxUniqueChannelNumbers; j++) {
      operating_channel_array[j] = kFakeChannel;
    }
    band_caps_buffer[i].operating_channel_list = operating_channel_array;
  }

  in.band_cap_list = band_caps_buffer;
  in.band_cap_count = wlan_common::kMaxBands;

  wlan_fullmac_query_info_t out;

  wlanif::ConvertQueryInfo(in, &out);

  EXPECT_EQ(0, memcmp(out.sta_addr, kFakeMacAddr, wlan_ieee80211::kMacAddrLen));
  EXPECT_EQ(kFakeBanjoMacRole, out.role);
  EXPECT_EQ(kFakeFeature, out.features);
  EXPECT_EQ(wlan_common::kMaxBands, out.band_cap_count);

  for (size_t i = 0; i < wlan_common::kMaxBands; i++) {
    auto band_cap = out.band_cap_list[i];
    EXPECT_EQ(kFakeBanjoBand, band_cap.band);
    EXPECT_EQ(wlan_ieee80211::kMaxSupportedBasicRates, band_cap.basic_rate_count);
    for (size_t j = 0; j < wlan_ieee80211::kMaxSupportedBasicRates; j++) {
      EXPECT_EQ(kFakeRate, band_cap.basic_rate_list[j]);
    }
    EXPECT_EQ(kPopulaterBool, band_cap.ht_supported);
    EXPECT_EQ(0, memcmp(band_cap.ht_caps.bytes, kFakeHtCapBytes, wlan_ieee80211::kHtCapLen));
    EXPECT_EQ(kPopulaterBool, band_cap.vht_supported);
    EXPECT_EQ(0, memcmp(band_cap.vht_caps.bytes, kFakeVhtCapBytes, wlan_ieee80211::kVhtCapLen));
    EXPECT_EQ(wlan_ieee80211::kMaxUniqueChannelNumbers, band_cap.operating_channel_count);
    for (size_t j = 0; j < wlan_ieee80211::kMaxUniqueChannelNumbers; j++) {
      EXPECT_EQ(kFakeChannel, band_cap.operating_channel_list[j]);
    }
  }
}

TEST_F(ConvertTest, ToBanjoMacSublayerSupport) {
  wlan_common::MacSublayerSupport in;
  mac_sublayer_support_t out;

  in.rate_selection_offload.supported = kPopulaterBool;
  in.data_plane.data_plane_type = kFakeFidlDataPlaneType;
  in.device.is_synthetic = kPopulaterBool;
  in.device.mac_implementation_type = kFakeFidlMacImplementationType;
  in.device.tx_status_report_supported = kPopulaterBool;

  wlanif::ConvertMacSublayerSupport(in, &out);

  EXPECT_EQ(kPopulaterBool, out.rate_selection_offload.supported);
  EXPECT_EQ(kFakeBanjoDataPlaneType, out.data_plane.data_plane_type);
  EXPECT_EQ(kPopulaterBool, out.device.is_synthetic);
  EXPECT_EQ(kFakeBanjoMacImplementationType, out.device.mac_implementation_type);
  EXPECT_EQ(kPopulaterBool, out.device.tx_status_report_supported);
}

TEST_F(ConvertTest, ToBanjoSecuritySupport) {
  wlan_common::SecuritySupport in;
  security_support_t out;

  in.sae.driver_handler_supported = kPopulaterBool;
  in.sae.sme_handler_supported = kPopulaterBool;
  in.mfp.supported = kPopulaterBool;

  wlanif::ConvertSecuritySupport(in, &out);

  EXPECT_EQ(kPopulaterBool, in.sae.driver_handler_supported);
  EXPECT_EQ(kPopulaterBool, in.sae.sme_handler_supported);
  EXPECT_EQ(kPopulaterBool, in.mfp.supported);
}

TEST_F(ConvertTest, ToBanjoSpectrumManagementSupport) {
  wlan_common::SpectrumManagementSupport in;
  spectrum_management_support_t out;
  in.dfs.supported = kPopulaterBool;
  wlanif::ConvertSpectrumManagementSupport(in, &out);
  EXPECT_EQ(kPopulaterBool, out.dfs.supported);
}

TEST_F(ConvertTest, ToBanjoIfaceCounterStats) {
  fuchsia_wlan_fullmac::wire::WlanFullmacIfaceCounterStats in;
  wlan_fullmac_iface_counter_stats_t out;
  in.rx_unicast_total = kRandomPopulaterUint64;
  in.rx_unicast_drop = kRandomPopulaterUint64;
  in.rx_multicast = kRandomPopulaterUint64;
  in.tx_total = kRandomPopulaterUint64;
  in.tx_drop = kRandomPopulaterUint64;

  wlanif::ConvertIfaceCounterStats(in, &out);

  EXPECT_EQ(kRandomPopulaterUint64, out.rx_unicast_total);
  EXPECT_EQ(kRandomPopulaterUint64, out.rx_unicast_drop);
  EXPECT_EQ(kRandomPopulaterUint64, out.rx_multicast);
  EXPECT_EQ(kRandomPopulaterUint64, out.tx_total);
  EXPECT_EQ(kRandomPopulaterUint64, out.tx_drop);
}

TEST_F(ConvertTest, ToBanjoNoiseFloorHistogram) {
  wlan_fullmac::WlanFullmacNoiseFloorHistogram in;
  wlan_fullmac_noise_floor_histogram_t out;
  wlan_fullmac_hist_bucket_t out_bucket_buffer[kFakeBucketSize];
  out.noise_floor_samples_list = out_bucket_buffer;

  in.hist_scope = kFakeFidlHistScope;
  in.antenna_id.freq = kFakeFidlAntennaFreq;
  in.antenna_id.index = kRandomPopulaterUint8;

  wlan_fullmac::WlanFullmacHistBucket bucket_array[kFakeBucketSize];
  for (size_t i = 0; i < kFakeBucketSize; i++) {
    bucket_array[i].bucket_index = kRandomPopulaterUint16;
    bucket_array[i].num_samples = kRandomPopulaterUint64;
  }
  in.noise_floor_samples = fidl::VectorView<wlan_fullmac::WlanFullmacHistBucket>::FromExternal(
      bucket_array, kFakeBucketSize);
  in.invalid_samples = kRandomPopulaterUint64;

  wlanif::ConvertNoiseFloorHistogram(in, &out);

  EXPECT_EQ(kFakeBanjoHistScope, out.hist_scope);
  EXPECT_EQ(kFakeBanjoAntennaFreq, out.antenna_id.freq);
  EXPECT_EQ(kRandomPopulaterUint8, out.antenna_id.index);
  for (size_t i = 0; i < kFakeBucketSize; i++) {
    EXPECT_EQ(kRandomPopulaterUint16, out.noise_floor_samples_list[i].bucket_index);
    EXPECT_EQ(kRandomPopulaterUint64, out.noise_floor_samples_list[i].num_samples);
  }
  EXPECT_EQ(kFakeBucketSize, out.noise_floor_samples_count);

  EXPECT_EQ(kRandomPopulaterUint64, out.invalid_samples);
}

TEST_F(ConvertTest, ToBanjoRxRateIndexHistogram) {
  wlan_fullmac::WlanFullmacRxRateIndexHistogram in;
  wlan_fullmac_rx_rate_index_histogram_t out;
  wlan_fullmac_hist_bucket_t out_bucket_buffer[kFakeBucketSize];
  out.rx_rate_index_samples_list = out_bucket_buffer;

  in.hist_scope = kFakeFidlHistScope;
  in.antenna_id.freq = kFakeFidlAntennaFreq;
  in.antenna_id.index = kRandomPopulaterUint8;

  wlan_fullmac::WlanFullmacHistBucket bucket_array[kFakeBucketSize];
  for (size_t i = 0; i < kFakeBucketSize; i++) {
    bucket_array[i].bucket_index = kRandomPopulaterUint16;
    bucket_array[i].num_samples = kRandomPopulaterUint64;
  }
  in.rx_rate_index_samples = fidl::VectorView<wlan_fullmac::WlanFullmacHistBucket>::FromExternal(
      bucket_array, kFakeBucketSize);
  in.invalid_samples = kRandomPopulaterUint64;

  wlanif::ConvertRxRateIndexHistogram(in, &out);

  EXPECT_EQ(kFakeBanjoHistScope, out.hist_scope);
  EXPECT_EQ(kFakeBanjoAntennaFreq, out.antenna_id.freq);
  EXPECT_EQ(kRandomPopulaterUint8, out.antenna_id.index);
  for (size_t i = 0; i < kFakeBucketSize; i++) {
    EXPECT_EQ(kRandomPopulaterUint16, out.rx_rate_index_samples_list[i].bucket_index);
    EXPECT_EQ(kRandomPopulaterUint64, out.rx_rate_index_samples_list[i].num_samples);
  }
  EXPECT_EQ(kFakeBucketSize, out.rx_rate_index_samples_count);

  EXPECT_EQ(kRandomPopulaterUint64, out.invalid_samples);
}

TEST_F(ConvertTest, ToBanjoRssiHistogram) {
  wlan_fullmac::WlanFullmacRssiHistogram in;
  wlan_fullmac_rssi_histogram_t out;
  wlan_fullmac_hist_bucket_t out_bucket_buffer[kFakeBucketSize];
  out.rssi_samples_list = out_bucket_buffer;

  in.hist_scope = kFakeFidlHistScope;
  in.antenna_id.freq = kFakeFidlAntennaFreq;
  in.antenna_id.index = kRandomPopulaterUint8;

  wlan_fullmac::WlanFullmacHistBucket bucket_array[kFakeBucketSize];
  for (size_t i = 0; i < kFakeBucketSize; i++) {
    bucket_array[i].bucket_index = kRandomPopulaterUint16;
    bucket_array[i].num_samples = kRandomPopulaterUint64;
  }
  in.rssi_samples = fidl::VectorView<wlan_fullmac::WlanFullmacHistBucket>::FromExternal(
      bucket_array, kFakeBucketSize);
  in.invalid_samples = kRandomPopulaterUint64;

  wlanif::ConvertRssiHistogram(in, &out);

  EXPECT_EQ(kFakeBanjoHistScope, out.hist_scope);
  EXPECT_EQ(kFakeBanjoAntennaFreq, out.antenna_id.freq);
  EXPECT_EQ(kRandomPopulaterUint8, out.antenna_id.index);
  for (size_t i = 0; i < kFakeBucketSize; i++) {
    EXPECT_EQ(kRandomPopulaterUint16, out.rssi_samples_list[i].bucket_index);
    EXPECT_EQ(kRandomPopulaterUint64, out.rssi_samples_list[i].num_samples);
  }
  EXPECT_EQ(kFakeBucketSize, out.rssi_samples_count);

  EXPECT_EQ(kRandomPopulaterUint64, out.invalid_samples);
}

TEST_F(ConvertTest, ToBanjoSnrHistogram) {
  wlan_fullmac::WlanFullmacSnrHistogram in;
  wlan_fullmac_snr_histogram_t out;
  wlan_fullmac_hist_bucket_t out_bucket_buffer[kFakeBucketSize];
  out.snr_samples_list = out_bucket_buffer;

  in.hist_scope = kFakeFidlHistScope;
  in.antenna_id.freq = kFakeFidlAntennaFreq;
  in.antenna_id.index = kRandomPopulaterUint8;

  wlan_fullmac::WlanFullmacHistBucket bucket_array[kFakeBucketSize];
  for (size_t i = 0; i < kFakeBucketSize; i++) {
    bucket_array[i].bucket_index = kRandomPopulaterUint16;
    bucket_array[i].num_samples = kRandomPopulaterUint64;
  }
  in.snr_samples = fidl::VectorView<wlan_fullmac::WlanFullmacHistBucket>::FromExternal(
      bucket_array, kFakeBucketSize);
  in.invalid_samples = kRandomPopulaterUint64;

  wlanif::ConvertSnrHistogram(in, &out);

  EXPECT_EQ(kFakeBanjoHistScope, out.hist_scope);
  EXPECT_EQ(kFakeBanjoAntennaFreq, out.antenna_id.freq);
  EXPECT_EQ(kRandomPopulaterUint8, out.antenna_id.index);
  for (size_t i = 0; i < kFakeBucketSize; i++) {
    EXPECT_EQ(kRandomPopulaterUint16, out.snr_samples_list[i].bucket_index);
    EXPECT_EQ(kRandomPopulaterUint64, out.snr_samples_list[i].num_samples);
  }
  EXPECT_EQ(kFakeBucketSize, out.snr_samples_count);

  EXPECT_EQ(kRandomPopulaterUint64, out.invalid_samples);
}

TEST_F(ConvertTest, ToBanjoFullmacScanResult) {
  fuchsia_wlan_fullmac::wire::WlanFullmacScanResult in;
  wlan_fullmac_scan_result_t out;

  in.txn_id = kRandomPopulaterUint64;
  in.timestamp_nanos = kFakeTime;

  memcpy(in.bss.bssid.data(), kFakeMacAddr, sizeof(kFakeMacAddr));
  in.bss.bss_type = kFakeFidlBssType;
  in.bss.beacon_period = kRandomPopulaterUint16;
  in.bss.capability_info = kRandomPopulaterUint16;
  in.bss.ies =
      fidl::VectorView<uint8_t>::FromExternal(const_cast<uint8_t*>(kFakeIeBytes), kFakeIeLen);
  in.bss.channel.primary = kFakeChannel;
  in.bss.channel.cbw = kFakeFidlChannelBandwidth;
  in.bss.channel.secondary80 = kFakeChannel;
  in.bss.rssi_dbm = kRandomPopulaterInt8;
  in.bss.snr_db = kRandomPopulaterInt8;

  wlanif::ConvertFullmacScanResult(in, &out);

  EXPECT_EQ(kRandomPopulaterUint64, out.txn_id);
  EXPECT_EQ(kFakeTime, out.timestamp_nanos);
  EXPECT_EQ(0, memcmp(kFakeMacAddr, out.bss.bssid, sizeof(kFakeMacAddr)));
  EXPECT_EQ(kFakeBanjoBssType, out.bss.bss_type);
  EXPECT_EQ(kRandomPopulaterUint16, out.bss.beacon_period);
  EXPECT_EQ(kRandomPopulaterUint16, out.bss.capability_info);
  EXPECT_EQ(0, memcmp(kFakeIeBytes, out.bss.ies_list, kFakeIeLen));
  EXPECT_EQ(kFakeIeLen, out.bss.ies_count);
  EXPECT_EQ(kFakeChannel, out.bss.channel.primary);
  EXPECT_EQ(kFakeBanjoChannelBandwidth, out.bss.channel.cbw);
  EXPECT_EQ(kFakeChannel, out.bss.channel.secondary80);
  EXPECT_EQ(kRandomPopulaterInt8, out.bss.rssi_dbm);
  EXPECT_EQ(kRandomPopulaterInt8, out.bss.snr_db);
}

TEST_F(ConvertTest, ToBanjoScanEnd) {
  wlan_fullmac::WlanFullmacScanEnd in;
  wlan_fullmac_scan_end_t out;

  in.txn_id = kRandomPopulaterUint64;
  in.code = kFakeFidlScanResult;

  wlanif::ConvertScanEnd(in, &out);

  EXPECT_EQ(kRandomPopulaterUint64, out.txn_id);
  EXPECT_EQ(kFakeBanjoScanResult, out.code);
}

TEST_F(ConvertTest, ToBanjoConnectConfirm) {
  wlan_fullmac::WlanFullmacConnectConfirm in;
  wlan_fullmac_connect_confirm_t out;

  memcpy(in.peer_sta_address.data(), kFakeMacAddr, sizeof(kFakeMacAddr));
  in.result_code = kFakeFidlStatusCode;
  in.association_id = kRandomPopulaterUint16;
  in.association_ies =
      fidl::VectorView<uint8_t>::FromExternal(const_cast<uint8_t*>(kFakeIeBytes), kFakeIeLen);

  wlanif::ConvertConnectConfirm(in, &out);

  EXPECT_EQ(0, memcmp(kFakeMacAddr, out.peer_sta_address, sizeof(kFakeMacAddr)));
  EXPECT_EQ(kFakeBanjoStatusCode, out.result_code);
  EXPECT_EQ(kRandomPopulaterUint16, out.association_id);
  EXPECT_EQ(0, memcmp(kFakeIeBytes, out.association_ies_list, kFakeIeLen));
  EXPECT_EQ(kFakeIeLen, out.association_ies_count);
}

TEST_F(ConvertTest, ToBanjoEapolConf) {
  wlan_fullmac::WlanFullmacEapolConfirm in;
  wlan_fullmac_eapol_confirm_t out;

  in.result_code = kFakeFidlEapolResult;
  memcpy(in.dst_addr.data(), kFakeMacAddr, sizeof(kFakeMacAddr));

  wlanif::ConvertEapolConf(in, &out);

  EXPECT_EQ(kFakeBanjoEapolResult, out.result_code);
  EXPECT_EQ(0, memcmp(kFakeMacAddr, out.dst_addr, sizeof(kFakeMacAddr)));
}

TEST_F(ConvertTest, ToBanjoSaeFrame) {
  wlan_fullmac::WlanFullmacSaeFrame in;
  wlan_fullmac_sae_frame_t out;

  memcpy(in.peer_sta_address.data(), kFakeMacAddr, sizeof(kFakeMacAddr));
  in.status_code = kFakeFidlStatusCode;
  in.seq_num = kRandomPopulaterUint16;

  constexpr size_t kSaeFieldsSize = 50;
  uint8_t sae_fields_buffer[kSaeFieldsSize];
  for (size_t i = 0; i < kSaeFieldsSize; i++) {
    sae_fields_buffer[i] = kRandomPopulaterUint8;
  }
  in.sae_fields = fidl::VectorView<uint8_t>::FromExternal(sae_fields_buffer, kSaeFieldsSize);

  wlanif::ConvertSaeFrame(in, &out);

  EXPECT_EQ(0, memcmp(kFakeMacAddr, out.peer_sta_address, sizeof(kFakeMacAddr)));
  EXPECT_EQ(kFakeBanjoStatusCode, out.status_code);
  EXPECT_EQ(kRandomPopulaterUint16, out.seq_num);
  EXPECT_EQ(0, memcmp(sae_fields_buffer, out.sae_fields_list, kSaeFieldsSize));
  EXPECT_EQ(kSaeFieldsSize, out.sae_fields_count);
}

TEST_F(ConvertTest, ToBanjoWmmParams) {
  wlan_common::WlanWmmParameters in;
  wlan_wmm_parameters_t out;

  in.apsd = kPopulaterBool;
  in.ac_be_params.ecw_min = kRandomPopulaterUint8;
  in.ac_be_params.ecw_max = kRandomPopulaterUint8;
  in.ac_be_params.aifsn = kRandomPopulaterUint8;
  in.ac_be_params.txop_limit = kRandomPopulaterUint16;
  in.ac_be_params.acm = kPopulaterBool;

  in.ac_bk_params.ecw_min = kRandomPopulaterUint8;
  in.ac_bk_params.ecw_max = kRandomPopulaterUint8;
  in.ac_bk_params.aifsn = kRandomPopulaterUint8;
  in.ac_bk_params.txop_limit = kRandomPopulaterUint16;
  in.ac_bk_params.acm = kPopulaterBool;

  in.ac_vi_params.ecw_min = kRandomPopulaterUint8;
  in.ac_vi_params.ecw_max = kRandomPopulaterUint8;
  in.ac_vi_params.aifsn = kRandomPopulaterUint8;
  in.ac_vi_params.txop_limit = kRandomPopulaterUint16;
  in.ac_vi_params.acm = kPopulaterBool;

  in.ac_vo_params.ecw_min = kRandomPopulaterUint8;
  in.ac_vo_params.ecw_max = kRandomPopulaterUint8;
  in.ac_vo_params.aifsn = kRandomPopulaterUint8;
  in.ac_vo_params.txop_limit = kRandomPopulaterUint16;
  in.ac_vo_params.acm = kPopulaterBool;

  wlanif::ConvertWmmParams(in, &out);

  EXPECT_EQ(out.apsd, kPopulaterBool);
  EXPECT_EQ(out.ac_be_params.ecw_min, kRandomPopulaterUint8);
  EXPECT_EQ(out.ac_be_params.ecw_max, kRandomPopulaterUint8);
  EXPECT_EQ(out.ac_be_params.aifsn, kRandomPopulaterUint8);
  EXPECT_EQ(out.ac_be_params.txop_limit, kRandomPopulaterUint16);
  EXPECT_EQ(out.ac_be_params.acm, kPopulaterBool);

  EXPECT_EQ(out.ac_bk_params.ecw_min, kRandomPopulaterUint8);
  EXPECT_EQ(out.ac_bk_params.ecw_max, kRandomPopulaterUint8);
  EXPECT_EQ(out.ac_bk_params.aifsn, kRandomPopulaterUint8);
  EXPECT_EQ(out.ac_bk_params.txop_limit, kRandomPopulaterUint16);
  EXPECT_EQ(out.ac_bk_params.acm, kPopulaterBool);

  EXPECT_EQ(out.ac_vi_params.ecw_min, kRandomPopulaterUint8);
  EXPECT_EQ(out.ac_vi_params.ecw_max, kRandomPopulaterUint8);
  EXPECT_EQ(out.ac_vi_params.aifsn, kRandomPopulaterUint8);
  EXPECT_EQ(out.ac_vi_params.txop_limit, kRandomPopulaterUint16);
  EXPECT_EQ(out.ac_vi_params.acm, kPopulaterBool);

  EXPECT_EQ(out.ac_vo_params.ecw_min, kRandomPopulaterUint8);
  EXPECT_EQ(out.ac_vo_params.ecw_max, kRandomPopulaterUint8);
  EXPECT_EQ(out.ac_vo_params.aifsn, kRandomPopulaterUint8);
  EXPECT_EQ(out.ac_vo_params.txop_limit, kRandomPopulaterUint16);
  EXPECT_EQ(out.ac_vo_params.acm, kPopulaterBool);
}
}  // namespace wlan::drivers
