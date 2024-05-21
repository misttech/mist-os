// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_POWER_POWER_DOMAIN_POWER_DOMAIN_VISITOR_H_
#define LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_POWER_POWER_DOMAIN_POWER_DOMAIN_VISITOR_H_

#include <fidl/fuchsia.hardware.power/cpp/fidl.h>
#include <lib/driver/devicetree/manager/visitor.h>
#include <lib/driver/devicetree/visitors/property-parser.h>

namespace power_domain_visitor_dt {

class PowerDomainVisitor : public fdf_devicetree::Visitor {
 public:
  static constexpr char kPowerDomainCells[] = "#power-domain-cells";
  static constexpr uint32_t kPowerDomainCellsSize = 1;
  static constexpr char kPowerDomains[] = "power-domains";

  PowerDomainVisitor();
  zx::result<> Visit(fdf_devicetree::Node& node,
                     const devicetree::PropertyDecoder& decoder) override;

  zx::result<> FinalizeNode(fdf_devicetree::Node& node) override;

 private:
  struct PowerController {
    fuchsia_hardware_power::DomainMetadata domain_info;
  };

  // Return an existing or a new instance of PowerController.
  PowerController& GetController(fdf_devicetree::Phandle phandle);

  // Helper to parse nodes with a reference to power-controller in "power-domains" property.
  zx::result<> ParseReferenceChild(fdf_devicetree::Node& child,
                                   fdf_devicetree::ReferenceNode& parent,
                                   fdf_devicetree::PropertyCells specifiers);

  zx::result<> AddChildNodeSpec(fdf_devicetree::Node& child, uint32_t domain_id);

  std::unique_ptr<fdf_devicetree::PropertyParser> parser_;
  // Mapping of power controller Phandle to its info.
  std::map<fdf_devicetree::Phandle, PowerController> power_controllers_;
};

}  // namespace power_domain_visitor_dt

#endif  // LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_POWER_POWER_DOMAIN_POWER_DOMAIN_VISITOR_H_
