// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/devicetree/visitors/interrupt-parser.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/driver/logging/cpp/structured_logger.h>
#include <zircon/errors.h>

#include <cstdint>

namespace fdf_devicetree {

InterruptParser::InterruptParser(ReferenceNodeMatchCallback node_matcher,
                                 ReferenceChildCallback child_callback)
    : ReferencePropertyParser(
          "interrupts-extended", "#interrupt-cells",
          [this](fdf_devicetree::ReferenceNode& node) { return this->node_matcher_(node); },
          [this](fdf_devicetree::Node& child, fdf_devicetree::ReferenceNode& parent,
                 fdf_devicetree::PropertyCells specifiers) {
            return this->child_callback_(child, parent, specifiers);
          }),
      node_matcher_(std::move(node_matcher)),
      child_callback_(std::move(child_callback)) {}

zx::result<> InterruptParser::Visit(fdf_devicetree::Node& node,
                                    const devicetree::PropertyDecoder& decoder) {
  auto status = ReferencePropertyParser::Visit(node, decoder);
  if (status.is_error()) {
    FDF_LOG(ERROR, "Interrupts-extended parser failed for node '%s - %s", node.name().c_str(),
            status.status_string());
    return status.take_error();
  }

  auto interrupts_property = node.properties().find("interrupts");
  if (interrupts_property == node.properties().end()) {
    return zx::ok();
  }

  // Find the interrupt parent.
  fdf_devicetree::ReferenceNode interrupt_parent(nullptr);
  fdf_devicetree::ParentNode current(&node);
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
                status.status_string());
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

  // Parse interrupt by calling the child callback, if the interrupt parent is related to the
  // current visitor.

  if (!node_matcher_(interrupt_parent)) {
    return zx::ok();
  }

  auto cell_width_prop = interrupt_parent.properties().find("#interrupt-cells");
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

  for (size_t idx = 0; idx < cell_count; idx += cell_width.value()) {
    PropertyCells interrupt = interrupts_property->second.AsBytes().subspan(
        idx * sizeof(uint32_t), (*cell_width) * sizeof(uint32_t));
    auto status = child_callback_(node, interrupt_parent, interrupt);
    if (status.is_error()) {
      FDF_LOG(ERROR, "Failed to parse interrupt elements of node '%s' - %s", node.name().c_str(),
              status.status_string());
      return status.take_error();
    }
  }

  return zx::ok();
}

}  // namespace fdf_devicetree
