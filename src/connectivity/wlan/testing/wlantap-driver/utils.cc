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

  size_t band_cap_count =
      std::min(tap_phy_config.bands.count(), static_cast<size_t>(wlan_common::kMaxBands));
  wlan_softmac::WlanSoftmacBandCapability band_cap_list[wlan_common::kMaxBands];
  // FIDL type conversion from WlantapPhyConfig to WlanSoftmacBandCapability.
  for (size_t i = 0; i < band_cap_count; i++) {
    band_cap_list[i].band = (tap_phy_config.bands)[i].band;

    if ((tap_phy_config.bands)[i].ht_caps != nullptr) {
      band_cap_list[i].ht_supported = true;
      band_cap_list[i].ht_caps.bytes = (tap_phy_config.bands)[i].ht_caps->bytes;
    } else {
      band_cap_list[i].ht_supported = false;
    }

    if ((tap_phy_config.bands)[i].vht_caps != nullptr) {
      band_cap_list[i].vht_supported = true;
      band_cap_list[i].vht_caps.bytes = tap_phy_config.bands[i].vht_caps->bytes;
    } else {
      band_cap_list[i].vht_supported = false;
    }

    band_cap_list[i].basic_rate_count =
        std::min<size_t>((tap_phy_config.bands)[i].rates.count(),
                         fuchsia_wlan_internal::wire::kMaxSupportedBasicRates);
    std::copy_n((tap_phy_config.bands)[i].rates.data(), band_cap_list[i].basic_rate_count,
                band_cap_list[i].basic_rate_list.begin());

    band_cap_list[i].operating_channel_count =
        std::min<size_t>((tap_phy_config.bands)[i].operating_channels.count(),
                         fuchsia_wlan_ieee80211_MAX_UNIQUE_CHANNEL_NUMBERS);
    std::copy_n((tap_phy_config.bands)[i].operating_channels.data(),
                band_cap_list[i].operating_channel_count,
                band_cap_list[i].operating_channel_list.begin());
  }

  auto band_cap_vec = std::vector<fuchsia_wlan_softmac::wire::WlanSoftmacBandCapability>(
      band_cap_list, band_cap_list + band_cap_count);
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
