// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "serial-port-visitor.h"

#include <fidl/fuchsia.hardware.serial/cpp/fidl.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/visitors/registration.h>
#include <lib/driver/logging/cpp/logger.h>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/serial/cpp/bind.h>

namespace serial_port_visitor_dt {

SerialPortVisitor::SerialPortVisitor() {
  fdf_devicetree::Properties properties = {};
  properties.emplace_back(std::make_unique<fdf_devicetree::Uint32ArrayProperty>(kSerialport));
  properties.emplace_back(std::make_unique<fdf_devicetree::ReferenceProperty>(kUarts, kUartCells));
  properties.emplace_back(std::make_unique<fdf_devicetree::StringListProperty>(kUartNames));
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

  zx::result<> result = ParseSerialPort(node, *parser_output);
  if (!result.is_ok()) {
    return result;
  }

  return zx::ok();
}

zx::result<> SerialPortVisitor::ParseSerialPort(fdf_devicetree::Node& node,
                                                fdf_devicetree::PropertyValues& properties) {
  if (properties.find(kSerialport) == properties.end()) {
    return zx::ok();
  }

  auto& controller = GetController(node.id());
  controller.serial_class = *properties.at(kSerialport)[0].AsUint32();

  fuchsia_hardware_serial::SerialPortInfo serial_port_info = {};
  serial_port_info.serial_class() =
      static_cast<fuchsia_hardware_serial::Class>(*properties.at(kSerialport)[0].AsUint32());
  serial_port_info.serial_vid() = *properties.at(kSerialport)[1].AsUint32();
  serial_port_info.serial_pid() = *properties.at(kSerialport)[2].AsUint32();

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

SerialPortVisitor::UartController& SerialPortVisitor::GetController(
    fdf_devicetree::NodeID node_id) {
  auto controller_iter = uart_controllers_.find(node_id);
  if (controller_iter == uart_controllers_.end()) {
    uart_controllers_[node_id] = UartController();
  }
  return uart_controllers_[node_id];
}

zx::result<> SerialPortVisitor::ParseReferenceChild(fdf_devicetree::Node& node,
                                                    fdf_devicetree::PropertyValues& properties) {
  if (properties.find(kUarts) == properties.end()) {
    return zx::ok();
  }

  size_t count = properties[kUarts].size();
  std::vector<std::optional<std::string>> uart_names(count);

  if (properties.find(kUartNames) != properties.end()) {
    if (properties[kUartNames].size() > count) {
      FDF_LOG(ERROR, "Node '%s' has %zu uart entries but has %zu uart names.", node.name().c_str(),
              count, properties[kUartNames].size());
      return zx::error(ZX_ERR_INVALID_ARGS);
    }

    size_t name_idx = 0;
    for (auto& name : properties[kUartNames]) {
      uart_names[name_idx++] = name.AsString();
    }
  }

  for (uint32_t index = 0; index < count; index++) {
    auto reference = properties[kUarts][index].AsReference();
    if (reference && uart_controllers_.contains(reference->first.id())) {
      auto& controller = uart_controllers_[reference->first.id()];
      zx::result<> result = AddChildNodeSpec(node, controller.serial_class, uart_names[index]);
      if (!result.is_ok()) {
        return result;
      }
    } else {
      FDF_LOG(ERROR, "Node '%s' has a invalid uarts property.", node.name().c_str());
    }
  }

  return zx::ok();
}

zx::result<> SerialPortVisitor::AddChildNodeSpec(fdf_devicetree::Node& child, uint32_t serial_class,
                                                 std::optional<std::string> uart_name) {
  auto uart_node = fuchsia_driver_framework::ParentSpec{{
      .bind_rules =
          {
              fdf::MakeAcceptBindRule(bind_fuchsia::PROTOCOL,
                                      bind_fuchsia_serial::BIND_PROTOCOL_DEVICE),
              fdf::MakeAcceptBindRule(bind_fuchsia::SERIAL_CLASS, serial_class),
          },
      .properties =
          {
              fdf::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_serial::BIND_PROTOCOL_DEVICE),
          },
  }};

  if (uart_name) {
    uart_node.properties().push_back(fdf::MakeProperty(bind_fuchsia_serial::NAME, *uart_name));
  }

  child.AddNodeSpec(uart_node);
  FDF_LOG(DEBUG, "Added uart node spec with class %d name '%s' to node '%s'", serial_class,
          uart_name ? uart_name->c_str() : "", child.name().c_str());

  return zx::ok();
}

zx::result<> SerialPortVisitor::FinalizeNode(fdf_devicetree::Node& node) {
  zx::result parser_output = parser_->Parse(node);
  if (parser_output.is_error()) {
    FDF_LOG(ERROR, "Serial port visitor parse failed for node '%s' : %s", node.name().c_str(),
            parser_output.status_string());
    return parser_output.take_error();
  }

  zx::result<> result = ParseReferenceChild(node, *parser_output);
  if (!result.is_ok()) {
    return result;
  }

  return zx::ok();
}

}  // namespace serial_port_visitor_dt

REGISTER_DEVICETREE_VISITOR(serial_port_visitor_dt::SerialPortVisitor);
