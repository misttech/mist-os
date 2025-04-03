// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/devicetree/visitors/driver-visitor.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/driver/logging/cpp/structured_logger.h>

namespace fdf_devicetree {

constexpr const char kCompatibleProp[] = "compatible";

bool DriverVisitor::is_match(
    const std::unordered_map<std::string_view, devicetree::PropertyValue>& properties) {
  auto property = properties.find(kCompatibleProp);
  if (property == properties.end()) {
    return false;
  }

  // Make sure value is a string.
  if (property->second.AsStringList() == std::nullopt) {
    FDF_SLOG(WARNING, "Node has invalid compatible property",
             KV("prop_len", property->second.AsBytes().size()));
    return false;
  }

  return compatible_matcher_(property->second.AsStringList().value());
}

zx::result<> DriverVisitor::Visit(Node& node, const devicetree::PropertyDecoder& decoder) {
  // If this node matches the driver, call the visitor.
  if (is_match(node.properties())) {
    return DriverVisit(node, decoder);
  }

  return zx::ok();
}

zx::result<> DriverVisitor::FinalizeNode(Node& node) {
  if (is_match(node.properties())) {
    return DriverFinalizeNode(node);
  }

  return zx::ok();
}

}  // namespace fdf_devicetree
