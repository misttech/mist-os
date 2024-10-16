// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_SPMI_CONTROLLERS_SPMI_SPMI_VISITOR_H_
#define LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_SPMI_CONTROLLERS_SPMI_SPMI_VISITOR_H_

#include <fidl/fuchsia.hardware.spmi/cpp/fidl.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/devicetree/visitors/driver-visitor.h>
#include <lib/driver/devicetree/visitors/property-parser.h>

#include <map>
#include <set>
#include <vector>

namespace spmi_dt {

class SpmiVisitor : public fdf_devicetree::Visitor {
 public:
  SpmiVisitor();

  zx::result<> Visit(fdf_devicetree::Node& node,
                     const devicetree::PropertyDecoder& decoder) override;

  zx::result<> FinalizeNode(fdf_devicetree::Node& node) override;

 private:
  struct SubTarget {
    std::vector<fuchsia_driver_framework::ParentSpec> parent_specs;
    bool has_reference_property = false;
  };

  zx::result<> ParseController(fdf_devicetree::Node& node);
  zx::result<> ParseReferenceProperty(fdf_devicetree::Node& node);
  zx::result<fuchsia_hardware_spmi::TargetInfo> ParseTarget(uint32_t controller_id,
                                                            uint32_t target_id,
                                                            fdf_devicetree::ChildNode& node);
  zx::result<std::vector<fuchsia_hardware_spmi::SubTargetInfo>> ParseSubTarget(
      uint32_t controller_id, const fuchsia_hardware_spmi::TargetInfo& parent,
      fdf_devicetree::ChildNode& node);

  static zx::result<> FinalizeSubTarget(const SubTarget& sub_target, fdf_devicetree::Node& node);
  zx::result<> FinalizeSubTargetReferences(const std::set<uint32_t>& sub_target_references,
                                           fdf_devicetree::Node& node);

  std::unique_ptr<fdf_devicetree::PropertyParser> spmi_parser_;
  // Maps sub-target node ID to SubTarget struct.
  std::map<uint32_t, SubTarget> sub_targets_;
  // Maps the ID of a node containing a reference property to all of the IDs of the sub-target nodes
  // that were referenced.
  std::map<uint32_t, std::set<uint32_t>> sub_target_references_;
};

}  // namespace spmi_dt

#endif  // LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_SPMI_CONTROLLERS_SPMI_SPMI_VISITOR_H_
