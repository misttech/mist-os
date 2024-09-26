// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_SPMI_CONTROLLERS_SPMI_SPMI_VISITOR_H_
#define LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_SPMI_CONTROLLERS_SPMI_SPMI_VISITOR_H_

#include <fidl/fuchsia.hardware.spmi/cpp/fidl.h>
#include <lib/driver/devicetree/visitors/driver-visitor.h>
#include <lib/driver/devicetree/visitors/property-parser.h>

#include <vector>

namespace spmi_dt {

class SpmiVisitor : public fdf_devicetree::Visitor {
 public:
  zx::result<> Visit(fdf_devicetree::Node& node,
                     const devicetree::PropertyDecoder& decoder) override {
    return zx::ok();
  }

  zx::result<> FinalizeNode(fdf_devicetree::Node& node) override;

 private:
  zx::result<fuchsia_hardware_spmi::TargetInfo> ParseTarget(
      uint32_t target_id, const fdf_devicetree::ChildNode& node) const;

  zx::result<std::vector<fuchsia_hardware_spmi::SubTargetInfo>> ParseSubTarget(
      const fuchsia_hardware_spmi::TargetInfo& parent, const fdf_devicetree::ChildNode& node) const;

  uint32_t controller_id_ = 0;
};

}  // namespace spmi_dt

#endif  // LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_SPMI_CONTROLLERS_SPMI_SPMI_VISITOR_H_
