// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_CLOCK_CONTROLLERS_CLOCKIMPL_VISITOR_CLOCKIMPL_VISITOR_H_
#define LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_CLOCK_CONTROLLERS_CLOCKIMPL_VISITOR_CLOCKIMPL_VISITOR_H_

#include <fidl/fuchsia.hardware.clockimpl/cpp/fidl.h>
#include <lib/driver/devicetree/visitors/driver-visitor.h>
#include <lib/driver/devicetree/visitors/reference-property.h>

#include <cstdint>
#include <string_view>

namespace clock_impl_dt {

class ClockImplVisitor : public fdf_devicetree::Visitor {
 public:
  ClockImplVisitor();

  zx::result<> FinalizeNode(fdf_devicetree::Node& node) override;

  zx::result<> Visit(fdf_devicetree::Node& node,
                     const devicetree::PropertyDecoder& decoder) override;

  // Helper to parse nodes with a reference to clock-controller in "clocks" property.
  zx::result<> ParseReferenceChild(fdf_devicetree::Node& child,
                                   fdf_devicetree::ReferenceNode& parent,
                                   fdf_devicetree::PropertyCells specifiers,
                                   std::optional<std::string> reference_name);

 private:
  struct ClockController {
    std::vector<uint8_t> clock_ids_metadata;
  };

  // Return an existing or a new instance of ClockController.
  ClockController& GetController(fdf_devicetree::Phandle phandle);

  // Helper to parse clock init hog to produce fuchsia_hardware_clockimpl::InitStep.
  zx::result<> ParseInitChild(fdf_devicetree::Node& child);

  zx::result<> AddChildNodeSpec(fdf_devicetree::Node& child, uint32_t id, std::string clock_name);

  bool is_match(std::string_view node_name);

  std::map<fdf_devicetree::Phandle, ClockController> clock_controllers_;
  fdf_devicetree::ReferencePropertyParser clock_parser_;
};

}  // namespace clock_impl_dt

#endif  // LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_CLOCK_CONTROLLERS_CLOCKIMPL_VISITOR_CLOCKIMPL_VISITOR_H_
