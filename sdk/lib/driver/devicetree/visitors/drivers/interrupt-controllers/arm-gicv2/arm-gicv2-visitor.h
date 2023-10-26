// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_INTERRUPT_CONTROLLERS_ARM_GICV2_ARM_GICV2_VISITOR_H_
#define LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_INTERRUPT_CONTROLLERS_ARM_GICV2_ARM_GICV2_VISITOR_H_

#include <lib/driver/devicetree/visitors/driver-visitor.h>
#include <lib/driver/devicetree/visitors/interrupt-parser.h>

namespace arm_gic_dt {

class ArmGicV2Visitor : public fdf_devicetree::DriverVisitor {
 public:
  explicit ArmGicV2Visitor();

  zx::result<> DriverVisit(fdf_devicetree::Node& node,
                           const devicetree::PropertyDecoder& decoder) override;

  zx::result<> ChildParser(fdf_devicetree::Node& child, fdf_devicetree::ReferenceNode& parent,
                           fdf_devicetree::PropertyCells interrupt_cells);

 private:
  fdf_devicetree::InterruptParser interrupt_parser_;
};

}  // namespace arm_gic_dt

#endif  // LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_INTERRUPT_CONTROLLERS_ARM_GICV2_ARM_GICV2_VISITOR_H_
