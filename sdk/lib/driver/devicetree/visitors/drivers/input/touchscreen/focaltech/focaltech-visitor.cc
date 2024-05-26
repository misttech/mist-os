// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "focaltech-visitor.h"

#include <lib/ddk/metadata.h>
#include <lib/driver/devicetree/visitors/registration.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/focaltech/focaltech.h>

namespace focaltech_visitor_dt {

FocaltechVisitor::FocaltechVisitor()
    : DriverVisitor(
          {"focaltech,ft3x27", "focaltech,ft6336", "focaltech,ft5726", "focaltech,ft5336"}) {
  fdf_devicetree::Properties properties = {};
  properties.emplace_back(std::make_unique<fdf_devicetree::StringProperty>(kCompatible, true));
  properties.emplace_back(std::make_unique<fdf_devicetree::BoolProperty>(kNeedsFirmware));
  parser_ = std::make_unique<fdf_devicetree::PropertyParser>(std::move(properties));
}

zx::result<> FocaltechVisitor::DriverVisit(fdf_devicetree::Node& node,
                                           const devicetree::PropertyDecoder& decoder) {
  auto parser_output = parser_->Parse(node);
  if (parser_output.is_error()) {
    FDF_LOG(ERROR, "Focaltech visitor failed for node '%s' : %s", node.name().c_str(),
            parser_output.status_string());
    return parser_output.take_error();
  }

  FocaltechMetadata device_info;
  std::string compatible = std::string(parser_output->at(kCompatible)[0].AsString().value());
  if (compatible == "focaltech,ft3x27") {
    device_info.device_id = FOCALTECH_DEVICE_FT3X27;
  } else if (compatible == "focaltech,ft6336") {
    device_info.device_id = FOCALTECH_DEVICE_FT6336;
  } else if (compatible == "focaltech,ft5726") {
    device_info.device_id = FOCALTECH_DEVICE_FT5726;
  } else if (compatible == "focaltech,ft5336") {
    device_info.device_id = FOCALTECH_DEVICE_FT5336;
  } else {
    FDF_LOG(INFO, "Unsupported device type '%s' in node '%s'. Not adding focaltech metadata.",
            compatible.c_str(), node.name().c_str());
    return zx::ok();
  }

  device_info.needs_firmware = parser_output->find(kNeedsFirmware) != parser_output->end();

  fuchsia_hardware_platform_bus::Metadata focaltech_metadata = {
      {.type = DEVICE_METADATA_PRIVATE,
       .data = std::vector<uint8_t>(
           reinterpret_cast<const uint8_t*>(&device_info),
           reinterpret_cast<const uint8_t*>(&device_info) + sizeof(device_info))}};

  node.AddMetadata(focaltech_metadata);

  FDF_LOG(DEBUG, "Added focaltech metadata to node '%s'", node.name().c_str());

  return zx::ok();
}

}  // namespace focaltech_visitor_dt

REGISTER_DEVICETREE_VISITOR(focaltech_visitor_dt::FocaltechVisitor);
