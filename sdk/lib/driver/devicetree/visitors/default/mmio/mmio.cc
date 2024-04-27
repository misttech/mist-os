// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include "lib/driver/devicetree/visitors/default/mmio/mmio.h"

#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/devicetree/devicetree.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/driver/logging/cpp/structured_logger.h>

namespace fdf {
using namespace fuchsia_driver_framework;
}

namespace fdf_devicetree {

constexpr const char kMmioProp[] = "reg";

zx::result<> MmioVisitor::Visit(Node& node, const devicetree::PropertyDecoder& decoder) {
  auto property = node.properties().find(kMmioProp);
  if (property == node.properties().end()) {
    FDF_LOG(DEBUG, "Node '%s' has no mmio properties.", node.name().c_str());
    return zx::ok();
  }

  // Make sure value is a register array.
  auto reg_props = property->second.AsReg(decoder);
  if (reg_props == std::nullopt) {
    FDF_SLOG(WARNING, "Node has invalid mmio property", KV("node_name", node.name()));
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  for (uint32_t i = 0; i < reg_props->size(); i++) {
    if ((*reg_props)[i].size()) {
      fuchsia_hardware_platform_bus::Mmio mmio;
      mmio.base() = decoder.TranslateAddress(*(*reg_props)[i].address());
      mmio.length() = (*reg_props)[i].size();
      node.AddMmio(std::move(mmio));
      FDF_LOG(DEBUG, "MMIO [0x%0lx, 0x%lx) added to node '%s'.", *mmio.base(),
              *mmio.base() + *mmio.length(), node.name().data());
    } else {
      FDF_LOG(DEBUG, "Node '%s' reg is not mmio.", node.name().data());
      break;
    }
  }

  return zx::ok();
}

}  // namespace fdf_devicetree
