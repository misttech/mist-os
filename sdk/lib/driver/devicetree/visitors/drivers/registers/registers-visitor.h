// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_REGISTERS_REGISTERS_VISITOR_H_
#define LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_REGISTERS_REGISTERS_VISITOR_H_

#include <fidl/fuchsia.hardware.registers/cpp/fidl.h>
#include <lib/driver/devicetree/visitors/driver-visitor.h>
#include <lib/driver/devicetree/visitors/reference-property.h>

#include "lib/driver/devicetree/manager/node.h"

namespace registers_dt {

class RegistersVisitor : public fdf_devicetree::DriverVisitor {
 public:
  RegistersVisitor();
  zx::result<> DriverFinalizeNode(fdf_devicetree::Node& node) override;
  zx::result<> ParseReferenceChild(fdf_devicetree::Node& child,
                                   fdf_devicetree::ReferenceNode& parent,
                                   fdf_devicetree::PropertyCells specifiers,
                                   std::optional<std::string> reference_name);

 private:
  using RegisterNames = std::string;
  using RegisterMap = std::map<RegisterNames, fuchsia_hardware_registers::RegistersMetadataEntry>;
  struct RegisterController {
    std::optional<bool> overlap_check_on;
    RegisterMap registers;
  };

  zx::result<> AddChildNodeSpec(fdf_devicetree::Node& child,
                                std::optional<std::string> register_name);

  std::map<fdf_devicetree::Phandle, RegisterController> register_controllers_;
  fdf_devicetree::ReferencePropertyParser register_parser_;
};

}  // namespace registers_dt

#endif  // LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_REGISTERS_REGISTERS_VISITOR_H_
