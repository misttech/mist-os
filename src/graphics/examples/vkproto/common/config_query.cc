// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <cstdlib>

#include "src/graphics/examples/vkproto/common/config.h"

namespace vkp {
std::optional<uint32_t> GetGpuVendorId() {
  static std::optional<uint32_t> gpu_vendor_id = ([]() {
    auto c = config::Config::TakeFromStartupHandle();
    uint32_t vendor_id_int = c.gpu_vendor_id();
    if (vendor_id_int != 0) {
      return std::optional<uint32_t>{vendor_id_int};
    }
    return std::optional<uint32_t>{};
  })();

  return gpu_vendor_id;
}

}  // namespace vkp
