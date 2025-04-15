// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "power-domain-visitor.h"

#include <lib/ddk/metadata.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/visitors/registration.h>
#include <lib/driver/logging/cpp/logger.h>

#include <vector>

#include <bind/fuchsia/hardware/power/cpp/bind.h>
#include <bind/fuchsia/power/cpp/bind.h>

namespace power_domain_visitor_dt {

class PowerDomainCells {
 public:
  explicit PowerDomainCells(fdf_devicetree::PropertyCells cells)
      : power_domain_cells_(cells, PowerDomainVisitor::kPowerDomainCellsSize) {}

  // 1st cell denotes the domain id.
  uint32_t domain_id() { return static_cast<uint32_t>(*power_domain_cells_[0][0]); }

 private:
  using PowerDomainCell =
      devicetree::PropEncodedArrayElement<PowerDomainVisitor::kPowerDomainCellsSize>;
  devicetree::PropEncodedArray<PowerDomainCell> power_domain_cells_;
};

PowerDomainVisitor::PowerDomainVisitor() {
  fdf_devicetree::Properties properties = {};
  properties.emplace_back(
      std::make_unique<fdf_devicetree::ReferenceProperty>(kPowerDomains, kPowerDomainCells));
  parser_ = std::make_unique<fdf_devicetree::PropertyParser>(std::move(properties));
}

zx::result<> PowerDomainVisitor::Visit(fdf_devicetree::Node& node,
                                       const devicetree::PropertyDecoder& decoder) {
  auto parser_output = parser_->Parse(node);
  if (parser_output.is_error()) {
    FDF_LOG(ERROR, "Power domain visitor failed for node '%s' : %s", node.name().c_str(),
            parser_output.status_string());
    return parser_output.take_error();
  }

  if (!parser_output->contains(kPowerDomains)) {
    return zx::ok();
  }

  for (auto& domain_reference : parser_output->at(kPowerDomains)) {
    if (zx::result result = ParseReferenceChild(node, domain_reference.AsReference()->first,
                                                domain_reference.AsReference()->second);
        result.is_error()) {
      return result;
    }
  }

  return zx::ok();
}

zx::result<> PowerDomainVisitor::AddChildNodeSpec(fdf_devicetree::Node& child, uint32_t domain_id) {
  auto power_node = fuchsia_driver_framework::ParentSpec2{{
      .bind_rules =
          {
              fdf::MakeAcceptBindRule2(bind_fuchsia_hardware_power::SERVICE,
                                       bind_fuchsia_hardware_power::SERVICE_ZIRCONTRANSPORT),
              fdf::MakeAcceptBindRule2(bind_fuchsia_power::POWER_DOMAIN, domain_id),
          },
      .properties =
          {
              fdf::MakeProperty2(bind_fuchsia_hardware_power::SERVICE,
                                 bind_fuchsia_hardware_power::SERVICE_ZIRCONTRANSPORT),
              fdf::MakeProperty2(bind_fuchsia_power::POWER_DOMAIN, domain_id),
          },
  }};
  FDF_LOG(INFO, "Added power domain (id: %d) parent to node '%s'", domain_id, child.name().c_str());
  child.AddNodeSpec(power_node);
  return zx::ok();
}

PowerDomainVisitor::PowerController& PowerDomainVisitor::GetController(
    fdf_devicetree::Phandle phandle) {
  const auto [controller_iter, success] = power_controllers_.insert({phandle, PowerController()});
  return controller_iter->second;
}

zx::result<> PowerDomainVisitor::ParseReferenceChild(fdf_devicetree::Node& child,
                                                     fdf_devicetree::ReferenceNode& parent,
                                                     fdf_devicetree::PropertyCells specifiers) {
  auto& controller = GetController(*parent.phandle());

  if (specifiers.size_bytes() != kPowerDomainCellsSize * sizeof(uint32_t)) {
    FDF_LOG(ERROR,
            "Power domain reference '%s' has incorrect number of specifiers (%lu) - expected %d.",
            child.name().c_str(), specifiers.size_bytes() / sizeof(uint32_t),
            kPowerDomainCellsSize);
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  auto cells = PowerDomainCells(specifiers);
  fuchsia_hardware_power::Domain domain;
  domain.id() = cells.domain_id();

  if (!controller.domain_info.domains()) {
    controller.domain_info.domains() = std::vector<fuchsia_hardware_power::Domain>();
  }

  // Insert if the domain id is not already present.
  auto it = std::find_if(
      controller.domain_info.domains()->begin(), controller.domain_info.domains()->end(),
      [&domain](const fuchsia_hardware_power::Domain& entry) { return entry.id() == domain.id(); });
  if (it == controller.domain_info.domains()->end()) {
    controller.domain_info.domains()->push_back(domain);
    FDF_LOG(INFO, "Power domain added (id: %u) added to controller '%s'", cells.domain_id(),
            parent.name().c_str());
  }

  return AddChildNodeSpec(child, cells.domain_id());
}

zx::result<> PowerDomainVisitor::FinalizeNode(fdf_devicetree::Node& node) {
  if (!node.phandle() || (power_controllers_.find(*node.phandle()) == power_controllers_.end())) {
    return zx::ok();
  }

  auto controller = power_controllers_.at(*node.phandle());

  if (controller.domain_info.domains()) {
    const fit::result encoded_domain_info = fidl::Persist(controller.domain_info);
    if (!encoded_domain_info.is_ok()) {
      FDF_LOG(ERROR, "Failed to encode Power domain metadata for node %s: %s", node.name().c_str(),
              encoded_domain_info.error_value().FormatDescription().c_str());
      return zx::error(encoded_domain_info.error_value().status());
    }
    fuchsia_hardware_platform_bus::Metadata controller_metadata = {{
        .id = std::to_string(DEVICE_METADATA_POWER_DOMAINS),
        .data = encoded_domain_info.value(),
    }};
    node.AddMetadata(std::move(controller_metadata));
    FDF_LOG(INFO, "Power domain metadata added to node '%s'", node.name().c_str());
  }

  return zx::ok();
}

}  // namespace power_domain_visitor_dt

REGISTER_DEVICETREE_VISITOR(power_domain_visitor_dt::PowerDomainVisitor);
