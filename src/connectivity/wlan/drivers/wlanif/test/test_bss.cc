// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/wlan/drivers/wlanif/test/test_bss.h"

#include <fuchsia/wlan/common/c/banjo.h>
#include <fuchsia/wlan/common/cpp/fidl.h>
#include <fuchsia/wlan/fullmac/c/banjo.h>
#include <fuchsia/wlan/ieee80211/cpp/fidl.h>
#include <fuchsia/wlan/internal/c/banjo.h>
#include <fuchsia/wlan/internal/cpp/fidl.h>
#include <fuchsia/wlan/mlme/cpp/fidl.h>

namespace wlan_fullmac_test {

namespace wlan_common = ::fuchsia::wlan::common;
namespace wlan_ieee80211 = ::fuchsia::wlan::ieee80211;
namespace wlan_mlme = ::fuchsia::wlan::mlme;

bss_description_t CreateBssDescription(wlan_channel_t channel) {
  wlan::common::MacAddr bssid(kBssid1);

  bss_description_t bss_desc = {};
  std::memcpy((void*)bss_desc.bssid, bssid.byte, wlan_ieee80211::MAC_ADDR_LEN);
  bss_desc.bss_type = BSS_TYPE_INFRASTRUCTURE;
  bss_desc.beacon_period = kBeaconPeriodTu;
  bss_desc.capability_info = 1 | 1 << 5;  // ESS and short preamble bits
  bss_desc.ies_list = (uint8_t*)kIes;
  bss_desc.ies_count = sizeof(kIes);
  bss_desc.channel.cbw = CHANNEL_BANDWIDTH_CBW20;
  bss_desc.channel.primary = channel.primary;

  bss_desc.rssi_dbm = -35;

  return bss_desc;
}

wlan_mlme::StartRequest CreateStartReq() {
  wlan_mlme::StartRequest req;
  std::vector<uint8_t> ssid(kSsid, kSsid + sizeof(kSsid));
  req.ssid = std::move(ssid);
  req.bss_type = wlan_common::BssType::INFRASTRUCTURE;
  req.beacon_period = kBeaconPeriodTu;
  req.dtim_period = kDtimPeriodTu;
  req.channel = kBssChannel.primary;
  req.rates = {0x82, 0x84, 0x8b, 0x96, 0x0c, 0x12, 0x18, 0x24, 0x30, 0x48, 0x60, 0x6c};
  req.mesh_id.resize(0);
  req.phy = wlan_common::WlanPhyType::ERP;
  req.rsne.emplace(std::vector<uint8_t>(kRsne, kRsne + sizeof(kRsne)));
  return req;
}

wlan_mlme::StopRequest CreateStopReq() {
  wlan_mlme::StopRequest req;
  req.ssid = std::vector<uint8_t>(kSsid, kSsid + sizeof(kSsid));
  return req;
}

wlan_fullmac_impl_base_connect_request CreateConnectReq() {
  wlan_fullmac_impl_base_connect_request req = {};

  req.selected_bss = CreateBssDescription(kBssChannel);
  req.connect_failure_timeout = kConnectFailureTimeout;
  req.auth_type = WLAN_AUTH_TYPE_OPEN_SYSTEM;
  req.security_ie_list = (uint8_t*)kRsne;
  req.security_ie_count = sizeof(kRsne);
  return req;
}

wlan_mlme::DeauthenticateRequest CreateDeauthenticateReq() {
  wlan::common::MacAddr bssid(kBssid1);
  wlan_mlme::DeauthenticateRequest req;
  std::memcpy(req.peer_sta_address.data(), bssid.byte, wlan::common::kMacAddrLen);
  req.reason_code = wlan_ieee80211::ReasonCode::UNSPECIFIED_REASON;
  return req;
}

wlan_mlme::DisassociateRequest CreateDisassociateReq() {
  wlan::common::MacAddr bssid(kBssid1);
  wlan_mlme::DisassociateRequest req;
  std::memcpy(req.peer_sta_address.data(), bssid.byte, wlan::common::kMacAddrLen);
  req.reason_code = wlan_ieee80211::ReasonCode::UNSPECIFIED_REASON;
  return req;
}

}  // namespace wlan_fullmac_test
