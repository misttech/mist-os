// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "serial-port-visitor.h"

#include <fidl/fuchsia.hardware.serial/cpp/fidl.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/devicetree/visitors/registration.h>
#include <lib/driver/logging/cpp/logger.h>

namespace serial_port_visitor_dt {

SerialPortVisitor::SerialPortVisitor() {
  fdf_devicetree::Properties properties = {};
  properties.emplace_back(std::make_unique<fdf_devicetree::Uint32ArrayProperty>(kSerialport));
  parser_ = std::make_unique<fdf_devicetree::PropertyParser>(std::move(properties));
}

zx::result<> SerialPortVisitor::Visit(fdf_devicetree::Node& node,
                                      const devicetree::PropertyDecoder& decoder) {
  zx::result parser_output = parser_->Parse(node);
  if (parser_output.is_error()) {
    FDF_LOG(ERROR, "Serial port visitor parse failed for node '%s' : %s", node.name().c_str(),
            parser_output.status_string());
    return parser_output.take_error();
  }

  if (parser_output->find(kSerialport) == parser_output->end()) {
    return zx::ok();
  }

  fuchsia_hardware_serial::SerialPortInfo serial_port_info = {};
  serial_port_info.serial_class() =
      static_cast<fuchsia_hardware_serial::Class>(*parser_output->at(kSerialport)[0].AsUint32());
  serial_port_info.serial_vid() = *parser_output->at(kSerialport)[1].AsUint32();
  serial_port_info.serial_pid() = *parser_output->at(kSerialport)[2].AsUint32();

  fit::result encoded = fidl::Persist(serial_port_info);
  if (encoded.is_error()) {
    FDF_LOG(ERROR, "Failed to encode serial metadata: %s",
            encoded.error_value().FormatDescription().c_str());
    return zx::error(encoded.error_value().status());
  }

  // TODO(b/385364946): Remove once no longer retrieved.
  fuchsia_hardware_platform_bus::Metadata metadata = {{
      .id = std::to_string(DEVICE_METADATA_SERIAL_PORT_INFO),
      .data = *encoded,
  }};

  FDF_LOG(DEBUG, "Added serial port metadata (class=%d, vid=%d, pid=%d) to node '%s'",
          serial_port_info.serial_class(), serial_port_info.serial_vid(),
          serial_port_info.serial_pid(), node.name().c_str());

  node.AddMetadata(metadata);

  fuchsia_hardware_platform_bus::Metadata metadata2 = {{
      .id = fuchsia_hardware_serial::SerialPortInfo::kSerializableName,
      .data = *std::move(encoded),
  }};
  node.AddMetadata(metadata2);

  return zx::ok();
}

}  // namespace serial_port_visitor_dt

REGISTER_DEVICETREE_VISITOR(serial_port_visitor_dt::SerialPortVisitor);
