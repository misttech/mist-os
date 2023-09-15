// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_TESTING_WLANTAP_DRIVER_UTILS_H_
#define SRC_CONNECTIVITY_WLAN_TESTING_WLANTAP_DRIVER_UTILS_H_

#include <fidl/fuchsia.wlan.softmac/cpp/driver/wire.h>
#include <fidl/fuchsia.wlan.tap/cpp/wire.h>
#include <fuchsia/wlan/common/cpp/fidl.h>

#include <string>

namespace wlan_tap = fuchsia_wlan_tap::wire;
namespace wlan_softmac = fuchsia_wlan_softmac::wire;
namespace wlan_common = fuchsia_wlan_common::wire;

namespace wlan {

std::string RoleToString(wlan_common::WlanMacRole role);
void ConvertTapPhyConfig(wlan_softmac::WlanSoftmacQueryResponse* resp,
                         const wlan_tap::WlantapPhyConfig& tap_phy_config, fidl::AnyArena& arena);
}  // namespace wlan

#endif  // SRC_CONNECTIVITY_WLAN_TESTING_WLANTAP_DRIVER_UTILS_H_
