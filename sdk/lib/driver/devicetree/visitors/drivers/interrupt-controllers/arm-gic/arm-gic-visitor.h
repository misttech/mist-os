// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_INTERRUPT_CONTROLLERS_ARM_GIC_ARM_GIC_VISITOR_H_
#define LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_INTERRUPT_CONTROLLERS_ARM_GIC_ARM_GIC_VISITOR_H_

#include <lib/driver/devicetree/visitors/driver-visitor.h>
#include <lib/driver/devicetree/visitors/interrupt-parser.h>

namespace arm_gic_dt {

class ArmGicVisitor : public fdf_devicetree::DriverVisitor {
 public:
  explicit ArmGicVisitor();

  zx::result<> Visit(fdf_devicetree::Node& node,
                     const devicetree::PropertyDecoder& decoder) override;

 private:
  zx::result<> ParseInterrupts(fdf_devicetree::Node& node,
                               std::vector<fdf_devicetree::PropertyValue>& interrupts);

  zx::result<> ParseInterrupt(fdf_devicetree::Node& child, fdf_devicetree::ReferenceNode& parent,
                              fdf_devicetree::PropertyCells interrupt_cells);

  fdf_devicetree::InterruptParser interrupt_parser_;
};

}  // namespace arm_gic_dt

#endif  // LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_INTERRUPT_CONTROLLERS_ARM_GIC_ARM_GIC_VISITOR_H_
