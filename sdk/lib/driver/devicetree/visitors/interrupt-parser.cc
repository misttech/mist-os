// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/devicetree/devicetree.h>
#include <lib/driver/devicetree/visitors/interrupt-parser.h>
#include <lib/driver/devicetree/visitors/property-parser.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/driver/logging/cpp/structured_logger.h>
#include <zircon/errors.h>

#include <cstdint>
#include <optional>
#include <utility>
#include <vector>

namespace fdf_devicetree {

namespace {

Properties MakeInterruptProperties() {
  Properties props = {};
  props.emplace_back(std::make_unique<ReferenceProperty>(InterruptParser::kInterruptsExtended,
                                                         InterruptParser::kInterruptCells,
                                                         /*required=*/false));
  props.emplace_back(
      std::make_unique<StringListProperty>(InterruptParser::kInterruptNames, /*required=*/false));
  return props;
}

}  // namespace

InterruptParser::InterruptParser() : PropertyParser(MakeInterruptProperties()) {}

zx::result<PropertyValues> InterruptParser::Parse(Node& node) {
  auto interrupt_values = PropertyParser::Parse(node);
  if (interrupt_values.is_error()) {
    FDF_LOG(ERROR, "Interrupts-extended parser failed for node '%s - %s", node.name().c_str(),
            interrupt_values.status_string());
    return interrupt_values.take_error();
  }

  // "interrupts-extended" takes precedence over "interrupts". Return if kInterruptsExtended
  // exists.
  if (interrupt_values->contains(kInterruptsExtended)) {
    return zx::ok(*interrupt_values);
  }

  // Return early if there are no "interrupts" property for this node.
  auto interrupts_property = node.properties().find(kInterrupts);
  if (interrupts_property == node.properties().end()) {
    return zx::ok(*interrupt_values);
  }

  // Find the interrupt parent.
  ReferenceNode interrupt_parent(nullptr);
  ParentNode current(&node);
  // Traverse the parent chain upwards until interrupt parent or interrupt controller is
  // encountered.
  while (current) {
    auto parent_prop = current.properties().find("interrupt-parent");
    if (parent_prop != current.properties().end()) {
      auto phandle = parent_prop->second.AsUint32();
      if (!phandle) {
        FDF_LOG(ERROR, "Invalid interrupt-parent property in node '%s", current.name().c_str());
        return zx::error(ZX_ERR_INVALID_ARGS);
      }
      auto result = node.GetReferenceNode(*phandle);
      if (result.is_error()) {
        FDF_LOG(ERROR, "Failed to get reference node for phandle %d - %s ", *phandle,
                result.status_string());
        return result.take_error();
      }
      interrupt_parent = *result;
      break;
    }

    auto controller_prop = current.properties().find("interrupt-controller");
    if (controller_prop != current.properties().end()) {
      interrupt_parent = current.MakeReferenceNode();
      break;
    }
    current = current.parent();
  }

  if (!interrupt_parent) {
    FDF_LOG(ERROR, "Interrupt parent not found for node '%s'", node.name().c_str());
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  auto cell_width_prop = interrupt_parent.properties().find(kInterruptCells);
  if (cell_width_prop == current.properties().end()) {
    FDF_LOG(
        ERROR,
        "Could not find the interrupt cells property in the in interrupt parent '%s' for node '%s",
        interrupt_parent.name().c_str(), node.name().c_str());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  auto cell_width = cell_width_prop->second.AsUint32();
  if (!cell_width) {
    FDF_LOG(ERROR, "Invalid interrupt cells property in the in interrupt parent '%s' for node '%s",
            interrupt_parent.name().c_str(), node.name().c_str());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  size_t cell_count = interrupts_property->second.AsBytes().size_bytes() / sizeof(uint32_t);

  if ((cell_count % cell_width.value()) != 0) {
    FDF_LOG(
        ERROR,
        "Invalid number of interrupt elements in node '%s. Interrupt cell size is %d and there are %zu extra entries.",
        node.name().c_str(), cell_width.value(), cell_count % cell_width.value());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  std::vector<PropertyValue> interrupt_references;
  for (size_t offset = 0; offset < cell_count; offset += cell_width.value()) {
    PropertyCells interrupt = interrupts_property->second.AsBytes().subspan(
        offset * sizeof(uint32_t), (*cell_width) * sizeof(uint32_t));
    interrupt_references.emplace_back(interrupt, interrupt_parent);
  }
  (*interrupt_values)[kInterruptsExtended] = std::move(interrupt_references);

  if (interrupt_values->contains(kInterruptNames)) {
    const size_t interrupt_count = (*interrupt_values)[kInterruptsExtended].size();
    const size_t interrupt_name_count = (*interrupt_values)[kInterruptNames].size();
    if (interrupt_count != interrupt_name_count) {
      FDF_LOG(ERROR, "Number of interrupts (%zu) doesn't match number of interrupt-names (%zu)",
              interrupt_count, interrupt_name_count);
      return zx::error(ZX_ERR_INVALID_ARGS);
    }
  }

  return zx::ok(*interrupt_values);
}

}  // namespace fdf_devicetree
