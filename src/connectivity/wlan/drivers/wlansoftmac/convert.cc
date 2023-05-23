// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "convert.h"

#include <fuchsia/wlan/ieee80211/cpp/fidl.h>
#include <fuchsia/wlan/stats/cpp/fidl.h>
#include <zircon/status.h>

#include <wlan/drivers/log.h>

#include "lib/fidl/cpp/wire/array.h"

namespace wlan {

// FIDL to banjo conversions.
zx_status_t ConvertMacRole(const fuchsia_wlan_common::wire::WlanMacRole& in, wlan_mac_role_t* out) {
  switch (in) {
    case fuchsia_wlan_common::wire::WlanMacRole::kAp:
      *out = WLAN_MAC_ROLE_AP;
      break;
    case fuchsia_wlan_common::wire::WlanMacRole::kClient:
      *out = WLAN_MAC_ROLE_CLIENT;
      break;
    case fuchsia_wlan_common::wire::WlanMacRole::kMesh:
      *out = WLAN_MAC_ROLE_MESH;
      break;
    default:
      lerror("WlanMacRole is not supported: %u", static_cast<uint32_t>(in));
      return ZX_ERR_INVALID_ARGS;
  }

  return ZX_OK;
}

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

zx_status_t ConvertWlanSoftmacQueryResponse(
    const fuchsia_wlan_softmac::wire::WlanSoftmacQueryResponse& in,
    wlan_softmac_query_response_t* out) {
  zx_status_t status;
  if (!(in.has_sta_addr() && in.has_mac_role() && in.has_supported_phys() &&
        in.has_hardware_capability() && in.has_band_caps())) {
    lerror("WlanSoftmacQueryResponse missing fields: %s %s %s %s %s.",
           in.has_sta_addr() ? "" : "sta_addr", in.has_mac_role() ? "" : "mac_role",
           in.has_supported_phys() ? "" : "supported_phys",
           in.has_hardware_capability() ? "" : "hardware_capability",
           in.has_band_caps() ? "" : "band_caps");
    return ZX_ERR_INVALID_ARGS;
  }

  memcpy(out->sta_addr, in.sta_addr().data(), fuchsia_wlan_ieee80211::wire::kMacAddrLen);

  if ((status = ConvertMacRole(in.mac_role(), &out->mac_role)) != ZX_OK) {
    return status;
  }

  for (size_t i = 0; i < in.supported_phys().count(); i++) {
    if ((status = ConvertWlanPhyType(in.supported_phys().data()[i],
                                     &out->supported_phys_list[i])) != ZX_OK) {
      lerror("WlanPhyType is not supported.");
      return status;
    }
  }
  out->supported_phys_count = static_cast<uint8_t>(in.supported_phys().count());
  out->hardware_capability = in.hardware_capability();

  for (size_t i = 0; i < in.band_caps().count(); i++) {
    if (!in.band_caps()[i].has_band()) {
      lerror("band value not populated");
      return ZX_ERR_INVALID_ARGS;
    }

    switch (in.band_caps()[i].band()) {
      case fuchsia_wlan_common::wire::WlanBand::kTwoGhz:
        out->band_caps_list[i].band = WLAN_BAND_TWO_GHZ;
        break;
      case fuchsia_wlan_common::wire::WlanBand::kFiveGhz:
        out->band_caps_list[i].band = WLAN_BAND_FIVE_GHZ;
        break;
      default:
        lerror("WlanBand is not supported: %hhu", static_cast<uint8_t>(in.band_caps()[i].band()));
        return ZX_ERR_NOT_SUPPORTED;
    }

    if (!in.band_caps()[i].has_basic_rate_list() || in.band_caps()[i].basic_rate_count() == 0) {
      out->band_caps_list[i].basic_rate_count = 0;
    } else {
      out->band_caps_list[i].basic_rate_count =
          static_cast<uint8_t>(in.band_caps()[i].basic_rate_count());
      memcpy(out->band_caps_list[i].basic_rate_list, in.band_caps()[i].basic_rate_list().data(),
             in.band_caps()[i].basic_rate_count());
    }

    if (!in.band_caps()[i].has_ht_caps()) {
      out->band_caps_list[i].ht_supported = false;
    } else {
      ConvertHtCapabilities(in.band_caps()[i].ht_caps(), &out->band_caps_list[i].ht_caps);
      out->band_caps_list[i].ht_supported = true;
    }

    if (!in.band_caps()[i].has_vht_caps()) {
      out->band_caps_list[i].vht_supported = false;
    } else {
      out->band_caps_list[i].vht_supported = true;
      ConvertVhtCapabilities(in.band_caps()[i].vht_caps(), &out->band_caps_list[i].vht_caps);
    }

    if (!in.band_caps()[i].has_operating_channel_list() ||
        in.band_caps()[i].has_operating_channel_count() == 0) {
      out->band_caps_list[i].operating_channel_count = 0;
    } else {
      out->band_caps_list[i].operating_channel_count =
          static_cast<uint16_t>(in.band_caps()[i].operating_channel_count());
      memcpy(out->band_caps_list[i].operating_channel_list,
             in.band_caps()[i].operating_channel_list().data(),
             sizeof(uint8_t) * in.band_caps()[i].operating_channel_count());
    }
  }

  out->band_caps_count = static_cast<uint8_t>(in.band_caps().count());
  return ZX_OK;
}

void ConvertDiscoverySuppport(const fuchsia_wlan_common::wire::DiscoverySupport& in,
                              discovery_support_t* out) {
  out->scan_offload.supported = in.scan_offload.supported;
  out->probe_response_offload.supported = in.probe_response_offload.supported;
}

zx_status_t ConvertMacSublayerSupport(const fuchsia_wlan_common::wire::MacSublayerSupport& in,
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
      lerror("DataPlaneType is not supported: %hhu",
             static_cast<uint8_t>(in.data_plane.data_plane_type));
      return ZX_ERR_INVALID_ARGS;
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
      lerror("MacImplementationType is not supported: %hhu",
             static_cast<uint8_t>(in.device.mac_implementation_type));
      return ZX_ERR_INVALID_ARGS;
  }

  out->device.tx_status_report_supported = in.device.tx_status_report_supported;
  return ZX_OK;
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

zx_status_t ConvertChannel(const wlan_channel_t& in, fuchsia_wlan_common::wire::WlanChannel* out) {
  out->primary = in.primary;
  out->secondary80 = in.secondary80;
  return ConvertChannelBandwidth(in.cbw, &out->cbw);
}

zx_status_t ConvertJoinBssRequest(const join_bss_request_t& in,
                                  fuchsia_wlan_common::wire::JoinBssRequest* out,
                                  fidl::AnyArena& arena) {
  auto builder = fuchsia_wlan_common::wire::JoinBssRequest::Builder(arena);
  builder.remote(in.remote);
  fidl::Array<uint8_t, fuchsia_wlan_ieee80211::wire::kMacAddrLen> bssid;
  memcpy(bssid.begin(), in.bssid, fuchsia_wlan_ieee80211::wire::kMacAddrLen);
  builder.bssid(bssid);
  builder.beacon_period(in.beacon_period);

  switch (in.bss_type) {
    case BSS_TYPE_UNKNOWN:
      builder.bss_type(fuchsia_wlan_common::wire::BssType::kUnknown);
      break;
    case BSS_TYPE_INFRASTRUCTURE:
      builder.bss_type(fuchsia_wlan_common::wire::BssType::kInfrastructure);
      break;
    case BSS_TYPE_INDEPENDENT:
      builder.bss_type(fuchsia_wlan_common::wire::BssType::kIndependent);
      break;
    case BSS_TYPE_MESH:
      builder.bss_type(fuchsia_wlan_common::wire::BssType::kMesh);
      break;
    case BSS_TYPE_PERSONAL:
      builder.bss_type(fuchsia_wlan_common::wire::BssType::kPersonal);
      break;
    default:
      lerror("BssType is not supported: %u", in.bss_type);
      return ZX_ERR_INVALID_ARGS;
  }

  *out = builder.Build();
  return ZX_OK;
}

void ConvertEnableBeaconing(const wlan_softmac_enable_beaconing_request_t& in,
                            fuchsia_wlan_softmac::wire::WlanSoftmacEnableBeaconingRequest* out,
                            fidl::AnyArena& arena) {
  auto builder = fuchsia_wlan_softmac::wire::WlanSoftmacEnableBeaconingRequest::Builder(arena);
  fuchsia_wlan_softmac::wire::WlanTxPacket packet_template;
  auto mac_frame_vec =
      std::vector<uint8_t>(in.packet_template.mac_frame_buffer,
                           in.packet_template.mac_frame_buffer + in.packet_template.mac_frame_size);
  packet_template.mac_frame = fidl::VectorView<uint8_t>(arena, mac_frame_vec);

  // Convert WlanTxInfo.
  packet_template.info.tx_flags = in.packet_template.info.tx_flags;
  packet_template.info.valid_fields = in.packet_template.info.valid_fields;
  packet_template.info.tx_vector_idx = in.packet_template.info.tx_vector_idx;
  switch (in.packet_template.info.phy) {
    case WLAN_PHY_TYPE_DSSS:
      packet_template.info.phy = fuchsia_wlan_common::wire::WlanPhyType::kDsss;
      break;
    case WLAN_PHY_TYPE_HR:
      packet_template.info.phy = fuchsia_wlan_common::wire::WlanPhyType::kHr;
      break;
    case WLAN_PHY_TYPE_OFDM:
      packet_template.info.phy = fuchsia_wlan_common::wire::WlanPhyType::kOfdm;
      break;
    case WLAN_PHY_TYPE_ERP:
      packet_template.info.phy = fuchsia_wlan_common::wire::WlanPhyType::kErp;
      break;
    case WLAN_PHY_TYPE_HT:
      packet_template.info.phy = fuchsia_wlan_common::wire::WlanPhyType::kHt;
      break;
    case WLAN_PHY_TYPE_DMG:
      packet_template.info.phy = fuchsia_wlan_common::wire::WlanPhyType::kDmg;
      break;
    case WLAN_PHY_TYPE_VHT:
      packet_template.info.phy = fuchsia_wlan_common::wire::WlanPhyType::kVht;
      break;
    case WLAN_PHY_TYPE_TVHT:
      packet_template.info.phy = fuchsia_wlan_common::wire::WlanPhyType::kTvht;
      break;
    case WLAN_PHY_TYPE_S1G:
      packet_template.info.phy = fuchsia_wlan_common::wire::WlanPhyType::kS1G;
      break;
    case WLAN_PHY_TYPE_CDMG:
      packet_template.info.phy = fuchsia_wlan_common::wire::WlanPhyType::kCdmg;
      break;
    case WLAN_PHY_TYPE_CMMG:
      packet_template.info.phy = fuchsia_wlan_common::wire::WlanPhyType::kCmmg;
      break;
    case WLAN_PHY_TYPE_HE:
      packet_template.info.phy = fuchsia_wlan_common::wire::WlanPhyType::kHe;
      break;
    default:
      linfo("Empty WlanPhyType: %u", in.packet_template.info.phy);
      // TODO(fxbug.dev/106984): In packet template, this field is allowed to be empty, but FIDL
      // doesn't allow out-of-range enum values. Since we don't define an empty value(e.g.
      // WlanPhyType::kNone) in this enum for now, use the first one as empty value here.
      packet_template.info.phy = fuchsia_wlan_common::wire::WlanPhyType::kDsss;
  }
  switch (in.packet_template.info.channel_bandwidth) {
    case CHANNEL_BANDWIDTH_CBW20:
      packet_template.info.channel_bandwidth = fuchsia_wlan_common::wire::ChannelBandwidth::kCbw20;
      break;
    case CHANNEL_BANDWIDTH_CBW40:
      packet_template.info.channel_bandwidth = fuchsia_wlan_common::wire::ChannelBandwidth::kCbw40;
      break;
    case CHANNEL_BANDWIDTH_CBW40BELOW:
      packet_template.info.channel_bandwidth =
          fuchsia_wlan_common::wire::ChannelBandwidth::kCbw40Below;
      break;
    case CHANNEL_BANDWIDTH_CBW80:
      packet_template.info.channel_bandwidth = fuchsia_wlan_common::wire::ChannelBandwidth::kCbw80;
      break;
    case CHANNEL_BANDWIDTH_CBW160:
      packet_template.info.channel_bandwidth = fuchsia_wlan_common::wire::ChannelBandwidth::kCbw160;
      break;
    case CHANNEL_BANDWIDTH_CBW80P80:
      packet_template.info.channel_bandwidth =
          fuchsia_wlan_common::wire::ChannelBandwidth::kCbw80P80;
      break;
    default:
      linfo("Empty ChannelBandwidth: %u", in.packet_template.info.channel_bandwidth);
      // TODO(fxbug.dev/106984): Similar to WlanPhyType, set the bandwidth to the lowest value when
      // higher layer doesn't indicate it. Print out a log to inform that the value was empty.
      packet_template.info.channel_bandwidth = fuchsia_wlan_common::wire::ChannelBandwidth::kCbw20;
  }
  packet_template.info.mcs = in.packet_template.info.mcs;

  builder.packet_template(packet_template);
  builder.tim_ele_offset(in.tim_ele_offset);
  builder.beacon_interval(in.beacon_interval);
  *out = builder.Build();
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

  fuchsia_hardware_wlan_associnfo::wire::WlanKeyType wlan_key_type;
  switch (in.key_type) {
    case WLAN_KEY_TYPE_PAIRWISE:
      wlan_key_type = fuchsia_hardware_wlan_associnfo::wire::WlanKeyType::kPairwise;
      break;
    case WLAN_KEY_TYPE_GROUP:
      wlan_key_type = fuchsia_hardware_wlan_associnfo::wire::WlanKeyType::kGroup;
      break;
    case WLAN_KEY_TYPE_IGTK:
      wlan_key_type = fuchsia_hardware_wlan_associnfo::wire::WlanKeyType::kIgtk;
      break;
    case WLAN_KEY_TYPE_PEER:
      wlan_key_type = fuchsia_hardware_wlan_associnfo::wire::WlanKeyType::kPeer;
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

void ConvertPassiveScanArgs(const wlan_softmac_start_passive_scan_request_t& in,
                            fuchsia_wlan_softmac::wire::WlanSoftmacStartPassiveScanRequest* out,
                            fidl::AnyArena& arena) {
  auto builder = fuchsia_wlan_softmac::wire::WlanSoftmacStartPassiveScanRequest::Builder(arena);
  auto channel_vec = std::vector<uint8_t>(in.channels_list, in.channels_list + in.channels_count);
  builder.channels(fidl::VectorView<uint8_t>(arena, channel_vec));
  builder.min_channel_time(in.min_channel_time);
  builder.max_channel_time(in.max_channel_time);
  builder.min_home_time(in.min_home_time);
  *out = builder.Build();
}

void ConvertActiveScanArgs(const wlan_softmac_start_active_scan_request_t& in,
                           fuchsia_wlan_softmac::wire::WlanSoftmacStartActiveScanRequest* out,
                           fidl::AnyArena& arena) {
  auto builder = fuchsia_wlan_softmac::wire::WlanSoftmacStartActiveScanRequest::Builder(arena);
  auto channel_vec = std::vector<uint8_t>(in.channels_list, in.channels_list + in.channels_count);
  builder.channels(fidl::VectorView<uint8_t>(arena, channel_vec));
  std::vector<fuchsia_wlan_ieee80211::wire::CSsid> ssids_data;

  for (size_t i = 0; i < in.ssids_count; i++) {
    fuchsia_wlan_ieee80211::wire::CSsid cssid;
    cssid.len = in.ssids_list[i].len;
    memcpy(cssid.data.begin(), in.ssids_list[i].data, in.ssids_list[i].len);
    ssids_data.push_back(cssid);
  }

  auto ssids = fidl::VectorView<fuchsia_wlan_ieee80211::wire::CSsid>(arena, ssids_data);
  builder.ssids(ssids);
  auto mac_header_vec =
      std::vector<uint8_t>(in.mac_header_buffer, in.mac_header_buffer + in.mac_header_size);
  builder.mac_header(fidl::VectorView<uint8_t>(arena, mac_header_vec));
  auto ies_vec = std::vector<uint8_t>(in.ies_buffer, in.ies_buffer + in.ies_size);
  builder.ies(fidl::VectorView<uint8_t>(arena, ies_vec));
  builder.min_channel_time(in.min_channel_time);
  builder.max_channel_time(in.max_channel_time);
  builder.min_home_time(in.min_home_time);
  builder.min_probes_per_channel(in.min_probes_per_channel);
  builder.max_probes_per_channel(in.max_probes_per_channel);
  *out = builder.Build();
}

}  // namespace wlan
