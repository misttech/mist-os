// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "convert.h"

#include <fidl/fuchsia.wlan.ieee80211/cpp/fidl.h>
#include <zircon/status.h>

#include <wlan/drivers/log.h>

#include "lib/fidl/cpp/wire/array.h"

namespace wlan {

// FIDL to banjo conversions.
zx_status_t ConvertWlanPhyType(const fuchsia_wlan_common::wire::WlanPhyType& in,
                               wlan_phy_type_t* out) {
  switch (in) {
    case fuchsia_wlan_common::wire::WlanPhyType::kDsss:
      *out = WLAN_PHY_TYPE_DSSS;
      break;
    case fuchsia_wlan_common::wire::WlanPhyType::kHr:
      *out = WLAN_PHY_TYPE_HR;
      break;
    case fuchsia_wlan_common::wire::WlanPhyType::kOfdm:
      *out = WLAN_PHY_TYPE_OFDM;
      break;
    case fuchsia_wlan_common::wire::WlanPhyType::kErp:
      *out = WLAN_PHY_TYPE_ERP;
      break;
    case fuchsia_wlan_common::wire::WlanPhyType::kHt:
      *out = WLAN_PHY_TYPE_HT;
      break;
    case fuchsia_wlan_common::wire::WlanPhyType::kDmg:
      *out = WLAN_PHY_TYPE_DMG;
      break;
    case fuchsia_wlan_common::wire::WlanPhyType::kVht:
      *out = WLAN_PHY_TYPE_VHT;
      break;
    case fuchsia_wlan_common::wire::WlanPhyType::kTvht:
      *out = WLAN_PHY_TYPE_TVHT;
      break;
    case fuchsia_wlan_common::wire::WlanPhyType::kS1G:
      *out = WLAN_PHY_TYPE_S1G;
      break;
    case fuchsia_wlan_common::wire::WlanPhyType::kCdmg:
      *out = WLAN_PHY_TYPE_CDMG;
      break;
    case fuchsia_wlan_common::wire::WlanPhyType::kCmmg:
      *out = WLAN_PHY_TYPE_CMMG;
      break;
    case fuchsia_wlan_common::wire::WlanPhyType::kHe:
      *out = WLAN_PHY_TYPE_HE;
      break;
    default:
      lerror("WlanPhyType is not supported: %u", static_cast<uint32_t>(in));
      return ZX_ERR_INVALID_ARGS;
  }

  return ZX_OK;
}

void ConvertHtCapabilities(const fuchsia_wlan_ieee80211::wire::HtCapabilities& in,
                           ht_capabilities_t* out) {
  memcpy(out->bytes, in.bytes.data(), fuchsia_wlan_ieee80211::wire::kHtCapLen);
}

void ConvertVhtCapabilities(const fuchsia_wlan_ieee80211::wire::VhtCapabilities& in,
                            vht_capabilities_t* out) {
  memcpy(out->bytes, in.bytes.data(), fuchsia_wlan_ieee80211::wire::kVhtCapLen);
}

zx_status_t ConvertMacRole(const wlan_mac_role_t& in, fuchsia_wlan_common::wire::WlanMacRole* out) {
  switch (in) {
    case WLAN_MAC_ROLE_AP:
      *out = fuchsia_wlan_common::wire::WlanMacRole::kAp;
      break;
    case WLAN_MAC_ROLE_CLIENT:
      *out = fuchsia_wlan_common::wire::WlanMacRole::kClient;
      break;
    case WLAN_MAC_ROLE_MESH:
      *out = fuchsia_wlan_common::wire::WlanMacRole::kMesh;
      break;
    default:
      lerror("WlanMacRole is not supported: %u", in);
      return ZX_ERR_INVALID_ARGS;
  }

  return ZX_OK;
}

zx_status_t ConvertChannelBandwidth(const channel_bandwidth_t& in,
                                    fuchsia_wlan_common::wire::ChannelBandwidth* out) {
  switch (in) {
    case CHANNEL_BANDWIDTH_CBW20:
      *out = fuchsia_wlan_common::wire::ChannelBandwidth::kCbw20;
      break;
    case CHANNEL_BANDWIDTH_CBW40:
      *out = fuchsia_wlan_common::wire::ChannelBandwidth::kCbw40;
      break;
    case CHANNEL_BANDWIDTH_CBW40BELOW:
      *out = fuchsia_wlan_common::wire::ChannelBandwidth::kCbw40Below;
      break;
    case CHANNEL_BANDWIDTH_CBW80:
      *out = fuchsia_wlan_common::wire::ChannelBandwidth::kCbw80;
      break;
    case CHANNEL_BANDWIDTH_CBW160:
      *out = fuchsia_wlan_common::wire::ChannelBandwidth::kCbw160;
      break;
    case CHANNEL_BANDWIDTH_CBW80P80:
      *out = fuchsia_wlan_common::wire::ChannelBandwidth::kCbw80P80;
      break;
    default:
      lerror("ChannelBandwidth is not supported: %u", in);
      return ZX_ERR_NOT_SUPPORTED;
  }

  return ZX_OK;
}

zx_status_t ConvertTxInfo(const wlan_tx_info_t& in, fuchsia_wlan_softmac::wire::WlanTxInfo* out) {
  out->tx_flags = in.tx_flags;
  out->valid_fields = in.valid_fields;
  out->tx_vector_idx = in.tx_vector_idx;
  switch (in.phy) {
    case WLAN_PHY_TYPE_DSSS:
      out->phy = fuchsia_wlan_common::wire::WlanPhyType::kDsss;
      break;
    case WLAN_PHY_TYPE_HR:
      out->phy = fuchsia_wlan_common::wire::WlanPhyType::kHr;
      break;
    case WLAN_PHY_TYPE_OFDM:
      out->phy = fuchsia_wlan_common::wire::WlanPhyType::kOfdm;
      break;
    case WLAN_PHY_TYPE_ERP:
      out->phy = fuchsia_wlan_common::wire::WlanPhyType::kErp;
      break;
    case WLAN_PHY_TYPE_HT:
      out->phy = fuchsia_wlan_common::wire::WlanPhyType::kHt;
      break;
    case WLAN_PHY_TYPE_DMG:
      out->phy = fuchsia_wlan_common::wire::WlanPhyType::kDmg;
      break;
    case WLAN_PHY_TYPE_VHT:
      out->phy = fuchsia_wlan_common::wire::WlanPhyType::kVht;
      break;
    case WLAN_PHY_TYPE_TVHT:
      out->phy = fuchsia_wlan_common::wire::WlanPhyType::kTvht;
      break;
    case WLAN_PHY_TYPE_S1G:
      out->phy = fuchsia_wlan_common::wire::WlanPhyType::kS1G;
      break;
    case WLAN_PHY_TYPE_CDMG:
      out->phy = fuchsia_wlan_common::wire::WlanPhyType::kCdmg;
      break;
    case WLAN_PHY_TYPE_CMMG:
      out->phy = fuchsia_wlan_common::wire::WlanPhyType::kCmmg;
      break;
    case WLAN_PHY_TYPE_HE:
      out->phy = fuchsia_wlan_common::wire::WlanPhyType::kHe;
      break;
    default:
      lerror("WlanPhyType is not supported: %u", in.phy);
      return ZX_ERR_INVALID_ARGS;
  }
  out->mcs = in.mcs;

  return ConvertChannelBandwidth(in.channel_bandwidth, &out->channel_bandwidth);
}

zx_status_t ConvertTxPacket(const uint8_t* data_in, const size_t data_len_in,
                            const wlan_tx_info_t& info_in,
                            fuchsia_wlan_softmac::wire::WlanTxPacket* out) {
  out->mac_frame =
      fidl::VectorView<uint8_t>::FromExternal(const_cast<uint8_t*>(data_in), data_len_in);

  return ConvertTxInfo(info_in, &out->info);
}

}  // namespace wlan
