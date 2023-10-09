// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.wlan.phyimpl/cpp/driver/wire.h>

#include <cstdio>

#include <wlan/common/phy.h>

namespace wlan {
namespace common {

std::string Alpha2ToStr(cpp20::span<const uint8_t> alpha2) {
  if (alpha2.size() != fuchsia_wlan_phyimpl::wire::kWlanphyAlpha2Len) {
    return "Invalid alpha2 length";
  }
  char buf[fuchsia_wlan_phyimpl::wire::kWlanphyAlpha2Len * 8 + 1];
  auto data = alpha2.data();
  bool is_printable = std::isprint(data[0]) && std::isprint(data[1]);
  if (is_printable) {
    snprintf(buf, sizeof(buf), "%c%c", data[0], data[1]);
  } else {
    snprintf(buf, sizeof(buf), "(%u)(%u)", data[0], data[1]);
  }
  return std::string(buf);
}

}  // namespace common
}  // namespace wlan
