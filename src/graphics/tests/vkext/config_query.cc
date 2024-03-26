// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "config_query.h"

#include <cstdlib>

#include "src/graphics/tests/common/vulkan_context.h"
#include "src/graphics/tests/vkext/config.h"

config::Config& GetConfig() {
  static auto config = config::Config::TakeFromStartupHandle();
  return config;
}

std::optional<uint32_t> GetGpuVendorId() {
  auto& c = GetConfig();
  std::string vendor_id_string = c.gpu_vendor_id();
  if (!vendor_id_string.empty()) {
    return std::optional<uint32_t>{strtol(vendor_id_string.c_str(), nullptr, 0)};
  }
  return std::optional<uint32_t>{};
}

bool SupportsSysmemYuv() {
  auto& c = GetConfig();
  return c.support_sysmem_yuv();
}

bool SupportsSysmemRenderableLinear() { return GetConfig().support_sysmem_renderable_linear(); }

bool SupportsSysmemLinearNonRgba() { return GetConfig().support_sysmem_linear_nonrgba(); }

bool SupportsProtectedMemory() { return GetConfig().support_protected_memory(); }