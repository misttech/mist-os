// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ethernet-phy-visitor.h"

#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/visitors/registration.h>
#include <lib/driver/logging/cpp/logger.h>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/ethernet/board/cpp/bind.h>

namespace eth_phy_visitor_dt {

EthPhyVisitor::EthPhyVisitor() {
  fdf_devicetree::Properties properties = {};
  properties.emplace_back(std::make_unique<fdf_devicetree::ReferenceProperty>(kPhys, kPhyCells));
  parser_ = std::make_unique<fdf_devicetree::PropertyParser>(std::move(properties));
}

bool EthPhyVisitor::is_match(const std::string& name) {
  return name.find("ethernet-phy") != std::string::npos;
}

zx::result<> EthPhyVisitor::Visit(fdf_devicetree::Node& node,
                                  const devicetree::PropertyDecoder& decoder) {
  zx::result parser_output = parser_->Parse(node);
  if (parser_output.is_error()) {
    FDF_LOG(ERROR, "Ethernet phy visitor parse failed for node '%s' : %s", node.name().c_str(),
            parser_output.status_string());
    return parser_output.take_error();
  }

  // ethernet-phy references only have one entry.
  if (!parser_output->contains(kPhys) || (*parser_output)[kPhys].size() != 1u) {
    return zx::ok();
  }

  auto reference = (*parser_output)[kPhys][0].AsReference();
  if (!reference) {
    FDF_LOG(ERROR, "Node '%s' has invalid phy reference.", node.name().c_str());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  if (!is_match(reference->first.name())) {
    // This reference is not to a ethernet-phy.
    return zx::ok();
  }

  auto result = AddChildNodeSpec(node);
  if (result.is_error()) {
    return result.take_error();
  }

  return zx::ok();
}

zx::result<> EthPhyVisitor::AddChildNodeSpec(fdf_devicetree::Node& child) {
  auto phy_node = fuchsia_driver_framework::ParentSpec{
      {.bind_rules =
           {
               fdf::MakeAcceptBindRule(
                   bind_fuchsia_hardware_ethernet_board::SERVICE,
                   bind_fuchsia_hardware_ethernet_board::SERVICE_ZIRCONTRANSPORT),
           },
       .properties = {
           fdf::MakeProperty(bind_fuchsia_hardware_ethernet_board::SERVICE,
                             bind_fuchsia_hardware_ethernet_board::SERVICE_ZIRCONTRANSPORT),
       }}};

  child.AddNodeSpec(phy_node);

  FDF_LOG(DEBUG, "Added ethernet phy bind rules to node '%s'.", child.name().c_str());
  return zx::ok();
}

}  // namespace eth_phy_visitor_dt

REGISTER_DEVICETREE_VISITOR(eth_phy_visitor_dt::EthPhyVisitor);
