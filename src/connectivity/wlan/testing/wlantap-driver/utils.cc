// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "utils.h"

#include <fuchsia/wlan/common/cpp/fidl.h>
#include <fuchsia/wlan/device/cpp/fidl.h>
#include <fuchsia/wlan/internal/cpp/fidl.h>
#include <lib/ddk/debug.h>

#include <wlan/common/band.h>
#include <wlan/common/channel.h>
#include <wlan/common/element.h>
#include <wlan/common/phy.h>

#include "fidl/fuchsia.wlan.common/cpp/common_types.h"
#include "fidl/fuchsia.wlan.ieee80211/cpp/natural_types.h"
#include "fidl/fuchsia.wlan.softmac/cpp/wire_types.h"
#include "lib/fidl/cpp/wire/array.h"

namespace wlan {

namespace wlan_common = fuchsia_wlan_common::wire;
namespace wlan_tap = fuchsia_wlan_tap::wire;

void ConvertTapPhyConfig(wlan_softmac::WlanSoftmacQueryResponse* resp,
                         const wlan_tap::WlantapPhyConfig& tap_phy_config, fidl::AnyArena& arena) {
  auto builder = wlan_softmac::WlanSoftmacQueryResponse::Builder(arena);

  builder.sta_addr(tap_phy_config.sta_addr);
  builder.mac_role(tap_phy_config.mac_role);
  builder.supported_phys(tap_phy_config.supported_phys);
  builder.hardware_capability(tap_phy_config.hardware_capability);

  size_t const band_cap_count =
      std::min(tap_phy_config.bands.count(), static_cast<size_t>(wlan_common::kMaxBands));

  std::vector<fuchsia_wlan_softmac::wire::WlanSoftmacBandCapability> band_cap_vec;

  // FIDL type conversion from WlantapPhyConfig to WlanSoftmacBandCapability.
  for (size_t i = 0; i < band_cap_count; i++) {
    auto band_cap_builder = fuchsia_wlan_softmac::wire::WlanSoftmacBandCapability::Builder(arena);

    band_cap_builder.band((tap_phy_config.bands)[i].band);

    if ((tap_phy_config.bands)[i].ht_caps != nullptr) {
      band_cap_builder.ht_caps((tap_phy_config.bands)[i].ht_caps);
    }

    if ((tap_phy_config.bands)[i].vht_caps != nullptr) {
      band_cap_builder.vht_caps(tap_phy_config.bands[i].vht_caps);
    }

    fidl::Array<uint8_t, fuchsia_wlan_ieee80211::wire::kMaxSupportedBasicRates> basic_rate_list;
    auto basic_rate_count = std::min<size_t>((tap_phy_config.bands)[i].rates.count(),
                                             fuchsia_wlan_ieee80211::wire::kMaxSupportedBasicRates);
    memcpy(basic_rate_list.begin(), (tap_phy_config.bands)[i].rates.begin(), basic_rate_count);
    band_cap_builder.basic_rate_list(basic_rate_list);
    band_cap_builder.basic_rate_count(basic_rate_count);

    fidl::Array<uint8_t, fuchsia_wlan_ieee80211::wire::kMaxUniqueChannelNumbers>
        operating_channel_list;
    auto operating_channel_count =
        std::min<size_t>((tap_phy_config.bands)[i].operating_channels.count(),
                         fuchsia_wlan_ieee80211_MAX_UNIQUE_CHANNEL_NUMBERS);
    memcpy(operating_channel_list.begin(), (tap_phy_config.bands)[i].operating_channels.begin(),
           operating_channel_count);
    band_cap_builder.operating_channel_list(operating_channel_list);
    band_cap_builder.operating_channel_count(operating_channel_count);

    band_cap_vec.push_back(band_cap_builder.Build());
  }

  builder.band_caps(
      fidl::VectorView<fuchsia_wlan_softmac::wire::WlanSoftmacBandCapability>(arena, band_cap_vec));

  *resp = builder.Build();
}

std::string RoleToString(wlan_common::WlanMacRole role) {
  switch (role) {
    case wlan_common::WlanMacRole::kClient:
      return "client";
    case wlan_common::WlanMacRole::kAp:
      return "ap";
    case wlan_common::WlanMacRole::kMesh:
      return "mesh";
    default:
      return "invalid";
  }
}

}  // namespace wlan
