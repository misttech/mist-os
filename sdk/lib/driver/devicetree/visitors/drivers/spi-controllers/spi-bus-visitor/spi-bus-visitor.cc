// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdk/lib/driver/devicetree/visitors/drivers/spi-controllers/spi-bus-visitor/spi-bus-visitor.h"

#include <lib/ddk/metadata.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/visitors/common-types.h>
#include <lib/driver/devicetree/visitors/registration.h>
#include <zircon/errors.h>

#include <algorithm>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/spi/cpp/bind.h>

namespace spi_bus_dt {

zx::result<> SpiBusVisitor::FinalizeNode(fdf_devicetree::Node& node) {
  if (!is_match(node)) {
    return zx::ok();
  }

  auto controller = spi_controllers_.find(node.name());
  ZX_ASSERT_MSG(controller != spi_controllers_.end(), "SPI controller '%s' entry not found",
                node.name().c_str());

  for (auto& child : node.children()) {
    if (zx::result<> result = ParseChild(controller->second, node, child); result.is_error()) {
      return result.take_error();
    }
  }

  if (controller->second.channels.empty()) {
    return zx::ok();
  }

  const fuchsia_hardware_spi_businfo::SpiBusMetadata bus_metadata = {{
      .channels = controller->second.channels,
      .bus_id = controller->second.bus_id,
  }};
  fit::result encoded_bus_metadata = fidl::Persist(bus_metadata);
  if (encoded_bus_metadata.is_error()) {
    FDF_LOG(INFO, "Failed to persist FIDL metadata for SPI controller '%s': %s",
            node.name().c_str(), encoded_bus_metadata.error_value().FormatDescription().c_str());
    return zx::error(encoded_bus_metadata.error_value().status());
  }
  // TODO(b/392676138): Don't add DEVICE_METADATA_SPI_CHANNELS once no longer retrieved.
  node.AddMetadata({{
      .id = std::to_string(DEVICE_METADATA_SPI_CHANNELS),
      .data = encoded_bus_metadata.value(),
  }});
  node.AddMetadata({{
      .id = fuchsia_hardware_spi_businfo::SpiBusMetadata::kSerializableName,
      .data = std::move(encoded_bus_metadata.value()),
  }});
  FDF_LOG(DEBUG, "SPI channels metadata added to node '%s'", node.name().c_str());
  return zx::ok();
}

zx::result<> SpiBusVisitor::Visit(fdf_devicetree::Node& node,
                                  const devicetree::PropertyDecoder& decoder) {
  if (is_match(node)) {
    if (zx::result<> result = CreateController(node.name()); result.is_error()) {
      return result.take_error();
    }
  }
  return zx::ok();
}

zx::result<> SpiBusVisitor::CreateController(const std::string& node_name) {
  auto controller_iter = spi_controllers_.find(node_name);
  if (controller_iter != spi_controllers_.end()) {
    FDF_LOG(ERROR, "Duplicate SPI controller '%s'", node_name.c_str());
    return zx::error(ZX_ERR_ALREADY_EXISTS);
  }

  spi_controllers_[node_name] = SpiController{.bus_id = bus_id_counter_++};
  return zx::ok();
}

void SpiBusVisitor::AddChildNodeSpec(fdf_devicetree::ChildNode& child, uint32_t bus_id,
                                     uint32_t chip_select, uint32_t child_chip_select_index) {
  child.AddNodeSpec(fuchsia_driver_framework::ParentSpec{{
      .bind_rules =
          {
              fdf::MakeAcceptBindRule(bind_fuchsia_hardware_spi::SERVICE,
                                      bind_fuchsia_hardware_spi::SERVICE_ZIRCONTRANSPORT),
              fdf::MakeAcceptBindRule(bind_fuchsia::SPI_BUS_ID, bus_id),
              fdf::MakeAcceptBindRule(bind_fuchsia::SPI_CHIP_SELECT, chip_select),
          },
      .properties =
          {
              fdf::MakeProperty(bind_fuchsia_hardware_spi::SERVICE,
                                bind_fuchsia_hardware_spi::SERVICE_ZIRCONTRANSPORT),
              // Add a chip select property, but zero-based for this child instead of dependent on
              // the number of children previously processed for this SPI controller.
              fdf::MakeProperty(bind_fuchsia::SPI_CHIP_SELECT, child_chip_select_index),
          },
  }});
}

zx::result<> SpiBusVisitor::ParseChild(SpiController& controller, fdf_devicetree::Node& parent,
                                       fdf_devicetree::ChildNode& child) {
  const auto property = child.properties().find("reg");
  if (property == child.properties().end()) {
    FDF_LOG(ERROR, "SPI child '%s' has no reg property", child.name().c_str());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  const fdf_devicetree::Uint32Array reg_props(property->second.AsBytes());
  for (uint32_t i = 0; i < reg_props.size(); i++) {
    const uint32_t chip_select = reg_props[i];

    const auto it =
        std::find_if(controller.channels.cbegin(), controller.channels.cend(),
                     [chip_select](const fuchsia_hardware_spi_businfo::SpiChannel& other) {
                       return other.cs() == chip_select;
                     });
    if (it != controller.channels.cend()) {
      FDF_LOG(ERROR, "Duplicate reg property %u for SPI controller '%s'", chip_select,
              parent.name().c_str());
      return zx::error(ZX_ERR_ALREADY_EXISTS);
    }

    FDF_LOG(DEBUG, "SPI channel %u to controller '%s'", chip_select, parent.name().c_str());
    controller.channels.emplace_back(fuchsia_hardware_spi_businfo::SpiChannel{{.cs = chip_select}});
    AddChildNodeSpec(child, controller.bus_id, chip_select, i);
  }

  return zx::ok();
}

bool SpiBusVisitor::is_match(fdf_devicetree::Node& node) {
  if (node.name().find("spi@") == std::string::npos) {
    return false;
  }

  const auto address_cells = node.properties().find("#address-cells");
  if (address_cells == node.properties().end() || address_cells->second.AsUint32() != 1) {
    return false;
  }

  const auto size_cells = node.properties().find("#size-cells");
  return size_cells != node.properties().end() && size_cells->second.AsUint32() == 0;
}

}  // namespace spi_bus_dt

REGISTER_DEVICETREE_VISITOR(spi_bus_dt::SpiBusVisitor);
