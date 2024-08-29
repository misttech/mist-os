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
constexpr const char kMmioNamesProp[] = "reg-names";

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

  devicetree::StringList<> names_list = [&] {
    auto names_property_it = node.properties().find(kMmioNamesProp);
    if (names_property_it == node.properties().end()) {
      FDF_LOG(DEBUG, "Node '%s' has no MMIO names properties.", node.name().c_str());
      return devicetree::StringList<>({});
    }

    std::optional<devicetree::StringList<>> names_property =
        names_property_it->second.AsStringList();
    if (names_property == std::nullopt) {
      FDF_LOG(DEBUG, "Node '%s' has invalid MMIO names properties.", node.name().c_str());
      return devicetree::StringList<>({});
    }

    return names_property.value();
  }();

  std::vector<std::string> mmio_names;
  for (const std::string_view& name : names_list) {
    mmio_names.emplace_back(name);
  }

  if (!mmio_names.empty() && mmio_names.size() != reg_props->size()) {
    FDF_LOG(ERROR, "Node '%s' has %zu MMIO regions but has %zu MMIO names.", node.name().c_str(),
            reg_props->size(), mmio_names.size());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  for (uint32_t i = 0; i < reg_props->size(); i++) {
    if ((*reg_props)[i].size()) {
      fuchsia_hardware_platform_bus::Mmio mmio;
      mmio.base() = decoder.TranslateAddress(*(*reg_props)[i].address());
      mmio.length() = (*reg_props)[i].size();
      if (!mmio_names.empty()) {
        mmio.name() = std::move(mmio_names[i]);
      }
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
