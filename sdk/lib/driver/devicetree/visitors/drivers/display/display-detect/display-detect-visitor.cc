// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "display-detect-visitor.h"

#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/visitors/registration.h>
#include <lib/driver/logging/cpp/logger.h>

#include <cstdint>
#include <memory>
#include <regex>

#include <bind/fuchsia/display/cpp/bind.h>

namespace display_detect_visitor_dt {

DisplayDetectVisitor::DisplayDetectVisitor() {
  fdf_devicetree::Properties properties = {};
  properties.emplace_back(
      std::make_unique<fdf_devicetree::ReferenceProperty>(kDisplayDetect, kDisplayDetectCells));
  properties.emplace_back(
      std::make_unique<fdf_devicetree::StringListProperty>(kDisplayDetectNames));
  parser_ = std::make_unique<fdf_devicetree::PropertyParser>(std::move(properties));
}

bool DisplayDetectVisitor::IsMatch(const std::string& node_name) {
  // Check that node name starts with "display-detect" and optionally contains a unit address (eg:
  // display-detect@ffaa0000).
  std::regex name_regex("^display-detect(@.*)?");
  return std::regex_match(node_name, name_regex);
}

zx::result<> DisplayDetectVisitor::Visit(fdf_devicetree::Node& node,
                                         const devicetree::PropertyDecoder& decoder) {
  zx::result<fdf_devicetree::PropertyValues> parser_output = parser_->Parse(node);
  if (parser_output.is_error()) {
    FDF_LOG(ERROR, "Display detect visitor parse failed for node '%s' : %s", node.name().c_str(),
            parser_output.status_string());
    return parser_output.take_error();
  }

  if (!parser_output->contains(kDisplayDetect)) {
    return zx::ok();
  }

  auto display_detect_it = parser_output->find(kDisplayDetect);
  if (display_detect_it == parser_output->end()) {
    return zx::ok();
  }

  const std::vector<fdf_devicetree::PropertyValue>& display_detect_properties =
      display_detect_it->second;

  auto display_detect_names_it = parser_output->find(kDisplayDetectNames);
  if (display_detect_names_it == parser_output->end()) {
    FDF_LOG(ERROR, "Node '%s' is missing display-detect-names.", node.name().c_str());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  const std::vector<fdf_devicetree::PropertyValue>& display_detect_names_properties =
      display_detect_names_it->second;

  size_t display_detect_count = display_detect_properties.size();
  if (display_detect_count != display_detect_names_properties.size()) {
    FDF_LOG(ERROR,
            "Node '%s' has an incorrect number of display-detect-names. Expected %zu, got %zu",
            node.name().c_str(), display_detect_count, display_detect_names_properties.size());
  }

  for (uint32_t index = 0; index < display_detect_count; index++) {
    auto reference = display_detect_properties[index].AsReference();

    if (!IsMatch(reference->first.name())) {
      continue;
    }

    auto name = display_detect_names_properties[index].AsString();

    auto result = AddChildNodeSpec(node, *name);
    if (result.is_error()) {
      return result.take_error();
    }
  }

  return zx::ok();
}

zx::result<> DisplayDetectVisitor::AddChildNodeSpec(fdf_devicetree::Node& child,
                                                    std::string_view output_name) {
  std::vector bind_rules = {
      fdf::MakeAcceptBindRule2(bind_fuchsia_display::OUTPUT,
                               "fuchsia.display.OUTPUT." + std::string(output_name)),

  };
  std::vector bind_properties = {fdf::MakeProperty2(
      bind_fuchsia_display::OUTPUT, "fuchsia.display.OUTPUT." + std::string(output_name))};

  auto display_detect_node = fuchsia_driver_framework::ParentSpec2{{bind_rules, bind_properties}};

  child.AddNodeSpec(display_detect_node);

  FDF_LOG(DEBUG, "Added '%s' bind rules to node '%s'.", std::string(output_name).c_str(),
          child.name().c_str());
  return zx::ok();
}

}  // namespace display_detect_visitor_dt

REGISTER_DEVICETREE_VISITOR(display_detect_visitor_dt::DisplayDetectVisitor);
