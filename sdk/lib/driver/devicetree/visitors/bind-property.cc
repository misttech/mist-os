// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include "sdk/lib/driver/devicetree/visitors/bind-property.h"

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/driver/logging/cpp/structured_logger.h>

#include <bind/fuchsia/devicetree/cpp/bind.h>

#include "sdk/lib/driver/devicetree/node.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}

namespace fdf_devicetree {

constexpr const char kCompatibleProp[] = "compatible";

zx::result<> BindPropertyVisitor::Visit(Node& node, const devicetree::PropertyDecoder& decoder) {
  auto property = node.properties().find(kCompatibleProp);
  if (property == node.properties().end()) {
    // TODO(fxbug.dev/107029): support extra "bind,..." properties as bind properties.
    FDF_LOG(DEBUG, "Node '%s' has no properties.", node.name().data());
    return zx::ok();
  }

  // Make sure value is a string.
  if (property->second.AsStringList() == std::nullopt) {
    FDF_SLOG(WARNING, "Node has invalid compatible property", KV("node_name", node.name()),
             KV("prop_len", property->second.AsBytes().size()));
    return zx::ok();
  }

  fdf::NodeProperty prop(
      fdf::NodePropertyKey::WithStringValue(bind_fuchsia_devicetree::FIRST_COMPATIBLE),
      fdf::NodePropertyValue::WithStringValue(
          std::string(*property->second.AsStringList()->begin())));

  FDF_LOG(DEBUG, "Added property %s to node '%s'", property->second.AsString()->data(),
          node.name().data());
  node.AddBindProperty(std::move(prop));

  return zx::ok();
}

}  // namespace fdf_devicetree
