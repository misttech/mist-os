// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "display-panel-visitor.h"

#include <lib/ddk/metadata.h>
#include <lib/device-protocol/display-panel.h>
#include <lib/driver/devicetree/visitors/registration.h>
#include <lib/driver/logging/cpp/logger.h>

#include <regex>

namespace display_panel_visitor_dt {

DisplayPanelVisitor::DisplayPanelVisitor() {
  fdf_devicetree::Properties properties = {};
  properties.emplace_back(std::make_unique<fdf_devicetree::Uint32Property>(kPanelType));
  properties.emplace_back(std::make_unique<fdf_devicetree::Uint32Property>(kDisplayWidth));
  properties.emplace_back(std::make_unique<fdf_devicetree::Uint32Property>(kDisplayHeight));
  parser_ = std::make_unique<fdf_devicetree::PropertyParser>(std::move(properties));
}

bool DisplayPanelVisitor::IsMatch(const std::string& node_name) {
  // Check that it contains "display" and optionally contains a unit address (eg:
  // hdmi-display@ffaa0000).
  std::regex name_regex(".*display(@.*)?$");
  return std::regex_match(node_name, name_regex);
}

zx::result<> DisplayPanelVisitor::Visit(fdf_devicetree::Node& node,
                                        const devicetree::PropertyDecoder& decoder) {
  if (!IsMatch(node.name())) {
    return zx::ok();
  }

  zx::result parser_output = parser_->Parse(node);
  if (parser_output.is_error()) {
    FDF_LOG(ERROR, "Display panel visitor parse failed for node '%s' : %s", node.name().c_str(),
            parser_output.status_string());
    return parser_output.take_error();
  }

  if (!parser_output->contains(kPanelType)) {
    return zx::ok();
  }

  display_panel_t display_panel_info;
  display_panel_info.panel_type = parser_output->at(kPanelType)[0].AsUint32().value();
  display_panel_info.width = parser_output->at(kDisplayWidth)[0].AsUint32().value();
  display_panel_info.height = parser_output->at(kDisplayHeight)[0].AsUint32().value();

  fuchsia_hardware_platform_bus::Metadata display_panel_metadata{{
      .id = std::to_string(DEVICE_METADATA_DISPLAY_PANEL_CONFIG),
      .data = std::vector<uint8_t>(
          reinterpret_cast<const uint8_t*>(&display_panel_info),
          reinterpret_cast<const uint8_t*>(&display_panel_info) + sizeof(display_panel_info)),
  }};

  node.AddMetadata(display_panel_metadata);

  FDF_LOG(DEBUG, "Display panel info - type(%d) size (%d x %d) added to node '%s'",
          display_panel_info.panel_type, display_panel_info.width, display_panel_info.height,
          node.name().c_str());

  return zx::ok();
}

}  // namespace display_panel_visitor_dt

REGISTER_DEVICETREE_VISITOR(display_panel_visitor_dt::DisplayPanelVisitor);
