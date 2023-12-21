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

zx_status_t ConvertRxInfo(const fuchsia_wlan_softmac::wire::WlanRxInfo& in, wlan_rx_info_t* out) {
  zx_status_t status;

  // WlanRxInfo class overrides uint32_t cast to return internal bitmap.
  out->rx_flags = static_cast<uint32_t>(in.rx_flags);
  out->valid_fields = static_cast<uint32_t>(in.valid_fields);

  if ((status = ConvertWlanPhyType(in.phy, &out->phy)) != ZX_OK) {
    lerror("WlanPhyType is not supported.");
    return status;
  }

  out->data_rate = in.data_rate;
  out->channel.primary = in.channel.primary;
  switch (in.channel.cbw) {
    case fuchsia_wlan_common::wire::ChannelBandwidth::kCbw20:
      out->channel.cbw = CHANNEL_BANDWIDTH_CBW20;
      break;
    case fuchsia_wlan_common::wire::ChannelBandwidth::kCbw40:
      out->channel.cbw = CHANNEL_BANDWIDTH_CBW40;
      break;
    case fuchsia_wlan_common::wire::ChannelBandwidth::kCbw40Below:
      out->channel.cbw = CHANNEL_BANDWIDTH_CBW40BELOW;
      break;
    case fuchsia_wlan_common::wire::ChannelBandwidth::kCbw80:
      out->channel.cbw = CHANNEL_BANDWIDTH_CBW80;
      break;
    case fuchsia_wlan_common::wire::ChannelBandwidth::kCbw160:
      out->channel.cbw = CHANNEL_BANDWIDTH_CBW160;
      break;
    case fuchsia_wlan_common::wire::ChannelBandwidth::kCbw80P80:
      out->channel.cbw = CHANNEL_BANDWIDTH_CBW80P80;
      break;
    default:
      lerror("ChannelBandwidth is not supported: %u", static_cast<uint32_t>(in.channel.cbw));
      return ZX_ERR_NOT_SUPPORTED;
  }
  out->channel.secondary80 = in.channel.secondary80;

  out->mcs = in.mcs;
  out->rssi_dbm = in.rssi_dbm;
  out->snr_dbh = in.snr_dbh;

  return ZX_OK;
}

zx_status_t ConvertRxPacket(const fuchsia_wlan_softmac::wire::WlanRxPacket& in,
                            wlan_rx_packet_t* out, uint8_t* rx_packet_buffer) {
  memcpy(rx_packet_buffer, in.mac_frame.begin(), in.mac_frame.count());
  out->mac_frame_buffer = rx_packet_buffer;
  out->mac_frame_size = in.mac_frame.count();

  return ConvertRxInfo(in.info, &out->info);
}

zx_status_t ConvertTxStatus(const fuchsia_wlan_common::wire::WlanTxResult& in,
                            wlan_tx_result_t* out) {
  for (size_t i = 0; i < fuchsia_wlan_common::wire::kWlanTxResultMaxEntry; i++) {
    out->tx_result_entry[i].tx_vector_idx = in.tx_result_entry.begin()[i].tx_vector_idx;
    out->tx_result_entry[i].attempts = in.tx_result_entry.begin()[i].attempts;
  }

  memcpy(&out->peer_addr[0], in.peer_addr.begin(), fuchsia_wlan_ieee80211::wire::kMacAddrLen);

  switch (in.result_code) {
    case fuchsia_wlan_common::wire::WlanTxResultCode::kFailed:
      out->result_code = WLAN_TX_RESULT_CODE_FAILED;
      break;
    case fuchsia_wlan_common::wire::WlanTxResultCode::kSuccess:
      out->result_code = WLAN_TX_RESULT_CODE_SUCCESS;
      break;
    default:
      lerror("WlanTxResult is not supported: %hhu", static_cast<uint8_t>(in.result_code));
      return ZX_ERR_INVALID_ARGS;
  }
  return ZX_OK;
}

// banjo to FIDL conversions.
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

zx_status_t ConvertKeyConfig(const wlan_key_configuration_t& in,
                             fuchsia_wlan_softmac::wire::WlanKeyConfiguration* out,
                             fidl::AnyArena& arena) {
  auto builder = fuchsia_wlan_softmac::wire::WlanKeyConfiguration::Builder(arena);
  fuchsia_wlan_softmac::wire::WlanProtection wlan_protection;

  // Enum conversion to WlanProtection.
  switch (in.protection) {
    case WLAN_PROTECTION_NONE:
      wlan_protection = fuchsia_wlan_softmac::wire::WlanProtection::kNone;
      break;
    case WLAN_PROTECTION_TX:
      wlan_protection = fuchsia_wlan_softmac::wire::WlanProtection::kTx;
      break;
    case WLAN_PROTECTION_RX:
      wlan_protection = fuchsia_wlan_softmac::wire::WlanProtection::kRx;
      break;
    case WLAN_PROTECTION_RX_TX:
      wlan_protection = fuchsia_wlan_softmac::wire::WlanProtection::kRxTx;
      break;
    default:
      lerror("WlanProtection is not supported: %hhu", in.protection);
      return ZX_ERR_INVALID_ARGS;
  }

  builder.protection(wlan_protection);

  fidl::Array<uint8_t, fuchsia_wlan_ieee80211::wire::kOuiLen> cipher_oui;

  memcpy(cipher_oui.begin(), &(in.cipher_oui[0]), fuchsia_wlan_ieee80211::wire::kOuiLen);
  builder.cipher_oui(cipher_oui);
  builder.cipher_type(in.cipher_type);

  fuchsia_wlan_common::wire::WlanKeyType wlan_key_type;
  switch (in.key_type) {
    case WLAN_KEY_TYPE_PAIRWISE:
      wlan_key_type = fuchsia_wlan_common::wire::WlanKeyType::kPairwise;
      break;
    case WLAN_KEY_TYPE_GROUP:
      wlan_key_type = fuchsia_wlan_common::wire::WlanKeyType::kGroup;
      break;
    case WLAN_KEY_TYPE_IGTK:
      wlan_key_type = fuchsia_wlan_common::wire::WlanKeyType::kIgtk;
      break;
    case WLAN_KEY_TYPE_PEER:
      wlan_key_type = fuchsia_wlan_common::wire::WlanKeyType::kPeer;
      break;
    default:
      lerror("WlanKeyType is not supported: %hhu", in.key_type);
      return ZX_ERR_INVALID_ARGS;
  }
  builder.key_type(wlan_key_type);

  fidl::Array<uint8_t, fuchsia_wlan_ieee80211::wire::kMacAddrLen> peer_addr;
  memcpy(peer_addr.begin(), in.peer_addr, fuchsia_wlan_ieee80211::wire::kMacAddrLen);
  builder.peer_addr(peer_addr);
  builder.key_idx(in.key_idx);

  auto key_vec = std::vector<uint8_t>(in.key_list, in.key_list + in.key_count);
  builder.key(fidl::VectorView<uint8_t>(arena, key_vec));
  builder.rsc(in.rsc);
  *out = builder.Build();
  return ZX_OK;
}

}  // namespace wlan
