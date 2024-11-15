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
constexpr const char kMemoryRegionProp[] = "memory-region";
constexpr const char kMemoryRegionNamesProp[] = "memory-region-names";

MmioVisitor::MmioVisitor() {
  fdf_devicetree::Properties properties = {};
  properties.emplace_back(std::make_unique<fdf_devicetree::StringListProperty>(kMmioNamesProp));
  properties.emplace_back(
      std::make_unique<fdf_devicetree::ReferenceProperty>(kMemoryRegionProp, 0u));
  properties.emplace_back(
      std::make_unique<fdf_devicetree::StringListProperty>(kMemoryRegionNamesProp));
  mmio_parser_ = std::make_unique<fdf_devicetree::PropertyParser>(std::move(properties));
}

zx::result<> MmioVisitor::RegPropertyParser(Node& node,
                                            fdf_devicetree::PropertyValues& parsed_props,
                                            const devicetree::PropertyDecoder& decoder) {
  auto property = node.properties().find(kMmioProp);
  if (property == node.properties().end()) {
    FDF_LOG(DEBUG, "Node '%s' has no reg properties.", node.name().c_str());
    return zx::ok();
  }

  // Make sure value is a register array.
  auto reg_props = property->second.AsReg(decoder);
  if (reg_props == std::nullopt) {
    FDF_SLOG(WARNING, "Node has invalid reg property", KV("node_name", node.name()));
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  std::vector<std::optional<std::string>> mmio_names(reg_props->size());
  if (parsed_props.find(kMmioNamesProp) != parsed_props.end()) {
    if (parsed_props[kMmioNamesProp].size() > reg_props->size()) {
      FDF_LOG(ERROR, "Node '%s' has %zu reg entries but has %zu reg names.", node.name().c_str(),
              reg_props->size(), parsed_props[kMmioNamesProp].size());
      return zx::error(ZX_ERR_INVALID_ARGS);
    }

    size_t name_idx = 0;
    for (auto& name : parsed_props[kMmioNamesProp]) {
      mmio_names[name_idx++] = name.AsString();
    }
  }

  for (uint32_t i = 0; i < reg_props->size(); i++) {
    if ((*reg_props)[i].size()) {
      fuchsia_hardware_platform_bus::Mmio mmio;
      mmio.base() = decoder.TranslateAddress(*(*reg_props)[i].address());
      mmio.length() = (*reg_props)[i].size();
      if (mmio_names[i]) {
        mmio.name() = std::move(*mmio_names[i]);
      }
      node_mmios_[node.id()].push_back(std::move(mmio));
    } else {
      FDF_LOG(DEBUG, "Node '%s' reg is not mmio.", node.name().data());
      break;
    }
  }

  return zx::ok();
}

zx::result<> MmioVisitor::MemoryRegionParser(Node& node,
                                             fdf_devicetree::PropertyValues& parsed_props) {
  if (parsed_props.find(kMemoryRegionProp) == parsed_props.end()) {
    return zx::ok();
  }
  size_t count = parsed_props[kMemoryRegionProp].size();
  std::vector<std::optional<std::string>> memory_region_names(count);
  if (parsed_props.find(kMemoryRegionNamesProp) != parsed_props.end()) {
    if (parsed_props[kMemoryRegionNamesProp].size() > count) {
      FDF_LOG(ERROR, "Node '%s' has %zu memory-region entries but has %zu memory-region names.",
              node.name().c_str(), count, parsed_props[kMemoryRegionNamesProp].size());
      return zx::error(ZX_ERR_INVALID_ARGS);
    }

    size_t name_idx = 0;
    for (auto& name : parsed_props[kMemoryRegionNamesProp]) {
      memory_region_names[name_idx++] = name.AsString();
    }
  }

  for (uint32_t index = 0; index < count; index++) {
    auto reference = parsed_props[kMemoryRegionProp][index].AsReference();
    if (reference) {
      std::pair<Node*, std::optional<std::string>> reference_info = {&node,
                                                                     memory_region_names[index]};
      memory_region_nodes_[reference->first.id()].push_back(std::move(reference_info));
    } else {
      FDF_LOG(ERROR, "Node '%s' has a invalid memory region property.", node.name().c_str());
    }
  }

  return zx::ok();
}

zx::result<> MmioVisitor::Visit(Node& node, const devicetree::PropertyDecoder& decoder) {
  zx::result parser_output = mmio_parser_->Parse(node);
  if (parser_output.is_error()) {
    FDF_LOG(ERROR, "Mmio visitor failed for node '%s' : %s", node.name().c_str(),
            parser_output.status_string());
    return parser_output.take_error();
  }

  zx::result result = RegPropertyParser(node, *parser_output, decoder);
  if (!result.is_ok()) {
    return result;
  }

  result = MemoryRegionParser(node, *parser_output);
  if (!result.is_ok()) {
    return result;
  }

  return zx::ok();
}

zx::result<> MmioVisitor::FinalizeNode(Node& node) {
  if (node.register_type() != RegisterType::kMmio ||
      node_mmios_.find(node.id()) == node_mmios_.end()) {
    return zx::ok();
  }

  if (memory_region_nodes_.find(node.id()) != memory_region_nodes_.end()) {
    for (auto& memory_region : memory_region_nodes_[node.id()]) {
      fdf_devicetree::Node* referee = memory_region.first;
      for (auto&& mmio : node_mmios_[node.id()]) {
        FDF_LOG(DEBUG, "Memory region [0x%0lx, 0x%lx) added to node '%s'.", *mmio.base(),
                *mmio.base() + *mmio.length(), referee->name().data());
        fuchsia_hardware_platform_bus::Mmio referee_mmio = mmio;
        if (memory_region.second) {
          referee_mmio.name() = *memory_region.second;
        }
        // Add the mmio to the referee.
        referee->AddMmio(std::move(referee_mmio));
      }
    }
  } else {
    for (auto&& mmio : node_mmios_[node.id()]) {
      FDF_LOG(DEBUG, "MMIO [0x%0lx, 0x%lx) added to node '%s'.", *mmio.base(),
              *mmio.base() + *mmio.length(), node.name().data());
      node.AddMmio(std::move(mmio));
    }
  }

  return zx::ok();
}

}  // namespace fdf_devicetree
