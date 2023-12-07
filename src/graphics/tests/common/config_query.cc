// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <cstdlib>

#include "src/graphics/tests/common/config.h"
#include "vulkan_context.h"

std::optional<uint32_t> GetGpuVendorId() {
  static std::optional<uint32_t> gpu_vendor_id = ([]() {
    auto c = config::Config::TakeFromStartupHandle();
    std::string vendor_id_string = c.gpu_vendor_id();
    if (!vendor_id_string.empty()) {
      return std::optional<uint32_t>{strtol(vendor_id_string.c_str(), nullptr, 0)};
    }
    return std::optional<uint32_t>{};
  })();

  return gpu_vendor_id;
}
