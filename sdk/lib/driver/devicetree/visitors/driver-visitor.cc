// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "driver-visitor.h"

#include <lib/driver/devicetree/node.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/driver/logging/cpp/structured_logger.h>

namespace fdf_devicetree {

constexpr const char kCompatibleProp[] = "compatible";

zx::result<> DriverVisitor::Visit(Node& node, const devicetree::PropertyDecoder& decoder) {
  auto property = node.properties().find(kCompatibleProp);
  if (property == node.properties().end()) {
    return zx::ok();
  }

  // Make sure value is a string.
  if (property->second.AsStringList() == std::nullopt) {
    FDF_SLOG(WARNING, "Node has invalid compatible property", KV("node_name", node.name()),
             KV("prop_len", property->second.AsBytes().size()));
    return zx::ok();
  }

  if (compatible_matcher_(*property->second.AsStringList()->begin())) {
    [[maybe_unused]] auto status = DriverVisit(node, decoder);
    return zx::ok();
  }

  return zx::ok();
}

}  // namespace fdf_devicetree
