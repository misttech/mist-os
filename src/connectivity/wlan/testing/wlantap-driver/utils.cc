// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "utils.h"

#include <fuchsia/wlan/common/cpp/fidl.h>
#include <lib/ddk/debug.h>

#include <wlan/common/channel.h>
#include <wlan/common/element.h>

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
      std::min(tap_phy_config.bands.size(), static_cast<size_t>(wlan_common::kMaxBands));

  std::vector<fuchsia_wlan_softmac::wire::WlanSoftmacBandCapability> softmac_band_caps;

  // FIDL type conversion from WlantapPhyConfig to WlanSoftmacBandCapability.
  for (size_t i = 0; i < band_cap_count; i++) {
    auto tap_band_caps = tap_phy_config.bands[i];
    auto softmac_band_caps_builder =
        fuchsia_wlan_softmac::wire::WlanSoftmacBandCapability::Builder(arena);

    softmac_band_caps_builder.band(tap_band_caps.band);

    if (tap_band_caps.ht_caps != nullptr) {
      softmac_band_caps_builder.ht_supported(true);
      softmac_band_caps_builder.ht_caps(tap_band_caps.ht_caps);
    }

    if (tap_band_caps.vht_caps != nullptr) {
      softmac_band_caps_builder.vht_supported(true);
      softmac_band_caps_builder.vht_caps(tap_band_caps.vht_caps);
    }

    auto basic_rate_count = std::min<size_t>(tap_band_caps.rates.size(),
                                             fuchsia_wlan_ieee80211::wire::kMaxSupportedBasicRates);
    std::vector<uint8_t> basic_rates(basic_rate_count);
    std::copy(tap_band_caps.rates.begin(), tap_band_caps.rates.begin() + basic_rate_count,
              basic_rates.begin());
    softmac_band_caps_builder.basic_rates(fidl::VectorView(arena, basic_rates));

    auto operating_channel_count =
        std::min<size_t>(tap_band_caps.operating_channels.size(),
                         fuchsia::wlan::ieee80211::MAX_UNIQUE_CHANNEL_NUMBERS);
    std::vector<uint8_t> operating_channels(operating_channel_count);
    std::copy(tap_band_caps.operating_channels.begin(),
              tap_band_caps.operating_channels.begin() + operating_channel_count,
              operating_channels.begin());
    softmac_band_caps_builder.operating_channels(fidl::VectorView(arena, operating_channels));

    softmac_band_caps.push_back(softmac_band_caps_builder.Build());
  }

  builder.band_caps(fidl::VectorView<fuchsia_wlan_softmac::wire::WlanSoftmacBandCapability>(
      arena, softmac_band_caps));

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
