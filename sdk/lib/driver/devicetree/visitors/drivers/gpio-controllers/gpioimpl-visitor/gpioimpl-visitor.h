// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_GPIO_CONTROLLERS_GPIOIMPL_VISITOR_GPIOIMPL_VISITOR_H_
#define LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_GPIO_CONTROLLERS_GPIOIMPL_VISITOR_GPIOIMPL_VISITOR_H_

#include <fidl/fuchsia.hardware.gpio/cpp/fidl.h>
#include <fidl/fuchsia.hardware.gpioimpl/cpp/fidl.h>
#include <lib/driver/devicetree/visitors/driver-visitor.h>
#include <lib/driver/devicetree/visitors/property-parser.h>

#include <cstdint>
#include <memory>

namespace gpio_impl_dt {

class GpioImplVisitor : public fdf_devicetree::Visitor {
 public:
  static constexpr char kGpioReference[] = "gpios";
  static constexpr char kGpioCells[] = "#gpio-cells";
  static constexpr char kGpioNames[] = "gpio-names";

  GpioImplVisitor();

  zx::result<> FinalizeNode(fdf_devicetree::Node& node) override;

  zx::result<> Visit(fdf_devicetree::Node& node,
                     const devicetree::PropertyDecoder& decoder) override;

 private:
  struct GpioController {
    std::vector<uint8_t> gpio_pins_metadata;
    fuchsia_hardware_gpioimpl::InitMetadata init_steps;
  };

  // Return an existing or a new instance of GpioController.
  GpioController& GetController(fdf_devicetree::Phandle phandle);

  // Helper to parse nodes with a reference to gpio-controller in "gpios" property.
  zx::result<> ParseReferenceChild(fdf_devicetree::Node& child,
                                   fdf_devicetree::ReferenceNode& parent,
                                   fdf_devicetree::PropertyCells specifiers,
                                   std::optional<std::string_view> gpio_name);

  // Helper to parse gpio init hog to produce fuchsia_hardware_gpioimpl::InitStep.
  zx::result<> ParseInitChild(fdf_devicetree::Node& child);

  zx::result<> AddChildNodeSpec(fdf_devicetree::Node& child, uint32_t pin, std::string gpio_name);

  bool is_match(const std::unordered_map<std::string_view, devicetree::PropertyValue>& properties);

  std::map<fdf_devicetree::Phandle, GpioController> gpio_controllers_;
  std::unique_ptr<fdf_devicetree::PropertyParser> gpio_parser_;
};

}  // namespace gpio_impl_dt

#endif  // LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_GPIO_CONTROLLERS_GPIOIMPL_VISITOR_GPIOIMPL_VISITOR_H_
