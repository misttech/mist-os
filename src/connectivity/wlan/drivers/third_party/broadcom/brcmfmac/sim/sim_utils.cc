// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/sim_utils.h"

#include <netinet/in.h>
#include <zircon/assert.h>

namespace wlan::brcmfmac::sim_utils {

zx_status_t WriteEthernetFrame(cpp20::span<uint8_t> out, common::MacAddr dst, common::MacAddr src,
                               uint16_t type, cpp20::span<const uint8_t> body) {
  if (out.size() < body.size() + kEthernetHeaderSize) {
    return ZX_ERR_INVALID_ARGS;
  }

  auto* hdr = reinterpret_cast<ethhdr*>(out.data());
  memcpy(hdr->h_dest, &dst, sizeof(dst));
  memcpy(hdr->h_source, &src, sizeof(src));
  hdr->h_proto = htons(type);
  memcpy(out.data() + kEthernetHeaderSize, body.data(), body.size());
  return ZX_OK;
}

std::vector<uint8_t> CreateEthernetFrame(common::MacAddr dst, common::MacAddr src, uint16_t type,
                                         cpp20::span<const uint8_t> body) {
  std::vector<uint8_t> out(body.size() + kEthernetHeaderSize);
  ZX_ASSERT(WriteEthernetFrame(out, dst, src, type, body) == ZX_OK);
  return out;
}

}  // namespace wlan::brcmfmac::sim_utils
