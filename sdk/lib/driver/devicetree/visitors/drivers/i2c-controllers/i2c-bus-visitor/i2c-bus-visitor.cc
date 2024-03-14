// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "i2c-bus-visitor.h"

#include <fidl/fuchsia.hardware.i2c.businfo/cpp/fidl.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/visitors/common-types.h>
#include <lib/driver/devicetree/visitors/registration.h>
#include <lib/driver/logging/cpp/logger.h>
#include <zircon/assert.h>
#include <zircon/errors.h>

#include <cstdint>
#include <optional>
#include <utility>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/i2c/cpp/bind.h>

namespace i2c_bus_dt {

bool I2cBusVisitor::is_match(fdf_devicetree::Node& node) {
  if (node.name().find("i2c-") == std::string::npos) {
    return false;
  }

  auto address_cells = node.properties().find("#address-cells");
  if (address_cells == node.properties().end() || address_cells->second.AsUint32() != 1) {
    return false;
  }

  auto size_cells = node.properties().find("#size-cells");
  if (size_cells == node.properties().end() || size_cells->second.AsUint32() != 0) {
    return false;
  }

  return true;
}

zx::result<> I2cBusVisitor::AddChildNodeSpec(fdf_devicetree::ChildNode& child, uint32_t bus_id,
                                             uint32_t address) {
  auto i2c_node = fuchsia_driver_framework::ParentSpec{{
      .bind_rules =
          {
              fdf::MakeAcceptBindRule(bind_fuchsia_hardware_i2c::SERVICE,
                                      bind_fuchsia_hardware_i2c::SERVICE_ZIRCONTRANSPORT),
              fdf::MakeAcceptBindRule(bind_fuchsia::I2C_BUS_ID, bus_id),
              fdf::MakeAcceptBindRule(bind_fuchsia::I2C_ADDRESS, address),
          },
      .properties =
          {
              fdf::MakeProperty(bind_fuchsia_hardware_i2c::SERVICE,
                                bind_fuchsia_hardware_i2c::SERVICE_ZIRCONTRANSPORT),
          },
  }};
  child.AddNodeSpec(i2c_node);
  return zx::ok();
}

zx::result<> I2cBusVisitor::CreateController(std::string node_name) {
  auto controller_iter = i2c_controllers_.find(node_name);
  if (controller_iter != i2c_controllers_.end()) {
    FDF_LOG(ERROR,
            "Failed to create I2C Controller. An I2C controller with name '%s' already exists.",
            node_name.c_str());
    return zx::error(ZX_ERR_ALREADY_EXISTS);
  }
  i2c_controllers_[node_name] = I2cController();
  i2c_controllers_[node_name].bus_id = bus_id_counter_++;
  return zx::ok();
}

zx::result<> I2cBusVisitor::ParseChild(I2cController& controller, fdf_devicetree::Node& parent,
                                       fdf_devicetree::ChildNode& child) {
  // Parse reg to get the address.
  auto property = child.properties().find("reg");
  if (property == child.properties().end()) {
    FDF_LOG(ERROR, "I2C child '%s' has no reg property.", child.name().c_str());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  auto reg_props = fdf_devicetree::Uint32Array(property->second.AsBytes());
  if (reg_props.size() != 1) {
    FDF_LOG(ERROR, "I2C child '%s' has incorrect reg property. Expected size 1, actual %zu",
            child.name().c_str(), reg_props.size());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  uint32_t address = reg_props[0];

  fuchsia_hardware_i2c_businfo::I2CChannel channel;
  channel.address() = address;
  channel.name() = child.name();
  FDF_LOG(DEBUG, "I2c channel added at address 0x%x to controller '%s'", address,
          parent.name().c_str());
  controller.channels.emplace_back(channel);
  return AddChildNodeSpec(child, controller.bus_id, address);
}

zx::result<> I2cBusVisitor::Visit(fdf_devicetree::Node& node,
                                  const devicetree::PropertyDecoder& decoder) {
  if (is_match(node)) {
    auto result = CreateController(node.name());
    if (result.is_error()) {
      return result.take_error();
    }
  }
  return zx::ok();
}

zx::result<> I2cBusVisitor::FinalizeNode(fdf_devicetree::Node& node) {
  // Check that it is indeed a i2c-controller that we support.
  if (!is_match(node)) {
    return zx::ok();
  }

  auto controller = i2c_controllers_.find(node.name());
  ZX_ASSERT_MSG(controller != i2c_controllers_.end(), "i2c controller '%s' entry not found.",
                node.name().c_str());

  for (auto& child : node.children()) {
    auto result = ParseChild(controller->second, node, child);
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to parse i2c child '%s' : %s", child.name().c_str(),
              result.status_string());
      return result.take_error();
    }
  }

  if (!controller->second.channels.empty()) {
    fuchsia_hardware_i2c_businfo::I2CBusMetadata bus_metadata = {{
        .channels = controller->second.channels,
        .bus_id = controller->second.bus_id,
    }};
    auto data = fidl::Persist(bus_metadata);
    if (data.is_error()) {
      FDF_LOG(INFO, "Failed to persist fidl metadata for i2c controller '%s': %s",
              node.name().c_str(), data.error_value().status_string());
      return zx::ok();
    }
    fuchsia_hardware_platform_bus::Metadata pbus_metadata = {{
        .type = DEVICE_METADATA_I2C_CHANNELS,
        .data = data.value(),
    }};
    node.AddMetadata(std::move(pbus_metadata));
    FDF_LOG(DEBUG, "I2C channels metadata added to node '%s'", node.name().c_str());
  }

  return zx::ok();
}

}  // namespace i2c_bus_dt

REGISTER_DEVICETREE_VISITOR(i2c_bus_dt::I2cBusVisitor);
