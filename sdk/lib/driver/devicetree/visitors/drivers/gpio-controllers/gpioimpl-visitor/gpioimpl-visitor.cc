// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "gpioimpl-visitor.h"

#include <fidl/fuchsia.hardware.gpioimpl/cpp/fidl.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/visitors/registration.h>
#include <lib/driver/logging/cpp/logger.h>
#include <zircon/assert.h>
#include <zircon/errors.h>

#include <cstdint>
#include <memory>
#include <optional>
#include <string_view>
#include <utility>
#include <vector>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <ddk/metadata/gpio.h>

namespace gpio_impl_dt {

namespace {
using fuchsia_hardware_gpio::GpioFlags;
using fuchsia_hardware_gpioimpl::InitCall;
using fuchsia_hardware_gpioimpl::InitMetadata;
using fuchsia_hardware_gpioimpl::InitStep;

class GpioCells {
 public:
  explicit GpioCells(fdf_devicetree::PropertyCells cells) : gpio_cells_(cells, 1, 1) {}

  // 1st cell denotes the gpio pin.
  uint32_t pin() { return static_cast<uint32_t>(*gpio_cells_[0][0]); }

  // 2nd cell represents GpioFlags. This is only used in gpio init hog nodes and ignored elsewhere.
  // TODO(b/314693127): The 2nd field definition is not yet finalized. Update this once the design
  // is complete.
  zx::result<GpioFlags> flags() {
    switch (static_cast<uint32_t>(*gpio_cells_[0][1])) {
      case 0:
        return zx::ok(GpioFlags::kPullDown);
      case 1:
        return zx::ok(GpioFlags::kPullUp);
      case 2:
        return zx::ok(GpioFlags::kNoPull);
      default:
        return zx::error(ZX_ERR_INVALID_ARGS);
    };
  }

 private:
  using GpioElement = devicetree::PropEncodedArrayElement<2>;
  devicetree::PropEncodedArray<GpioElement> gpio_cells_;
};

}  // namespace

// TODO(b/314693127): Name of the reference property can be *-gpios.
GpioImplVisitor::GpioImplVisitor() {
  fdf_devicetree::Properties properties = {};
  properties.emplace_back(
      std::make_unique<fdf_devicetree::ReferenceProperty>(kGpioReference, kGpioCells));
  properties.emplace_back(std::make_unique<fdf_devicetree::StringListProperty>(kGpioNames));
  gpio_parser_ = std::make_unique<fdf_devicetree::PropertyParser>(std::move(properties));
}

bool GpioImplVisitor::is_match(
    const std::unordered_map<std::string_view, devicetree::PropertyValue>& properties) {
  auto controller = properties.find("gpio-controller");
  return controller != properties.end();
}

zx::result<> GpioImplVisitor::Visit(fdf_devicetree::Node& node,
                                    const devicetree::PropertyDecoder& decoder) {
  auto gpio_hog = node.properties().find("gpio-hog");

  if (gpio_hog != node.properties().end()) {
    // Node containing gpio-hog property are to be parsed differently. They will be used to
    // construct gpio init step metadata.
    auto result = ParseInitChild(node);
    if (result.is_error()) {
      FDF_LOG(ERROR, "Gpio visitor failed for node '%s' : %s", node.name().c_str(),
              result.status_string());
    }
  } else {
    auto parser_output = gpio_parser_->Parse(node);

    if (parser_output->find(kGpioReference) == parser_output->end()) {
      return zx::ok();
    }

    if (parser_output->find(kGpioNames) == parser_output->end() ||
        (*parser_output)[kGpioReference].size() != (*parser_output)[kGpioReference].size()) {
      // We need a clock names to generate bind rules.
      FDF_LOG(ERROR, "Gpio reference '%s' does not have valid gpio names field.",
              node.name().c_str());
      return zx::error(ZX_ERR_INVALID_ARGS);
    }

    for (uint32_t index = 0; index < (*parser_output)[kGpioReference].size(); index++) {
      auto reference = (*parser_output)[kGpioReference][index].AsReference();
      if (reference && is_match(reference->first.properties())) {
        auto result = ParseReferenceChild(node, reference->first, reference->second,
                                          (*parser_output)[kGpioNames][index].AsString());
        if (result.is_error()) {
          return result.take_error();
        }
      }
    }
  }

  return zx::ok();
}

zx::result<> GpioImplVisitor::AddChildNodeSpec(fdf_devicetree::Node& child, uint32_t pin,
                                               std::string gpio_name) {
  auto gpio_node = fuchsia_driver_framework::ParentSpec{{
      .bind_rules =
          {
              fdf::MakeAcceptBindRule(bind_fuchsia::PROTOCOL,
                                      bind_fuchsia_gpio::BIND_PROTOCOL_DEVICE),
              fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN, pin),
          },
      .properties =
          {
              fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL,
                                bind_fuchsia_gpio::BIND_PROTOCOL_DEVICE),
              fdf::MakeProperty(bind_fuchsia_gpio::FUNCTION, "fuchsia.gpio.FUNCTION." + gpio_name),
          },
  }};
  child.AddNodeSpec(gpio_node);
  return zx::ok();
}

zx::result<> GpioImplVisitor::ParseInitChild(fdf_devicetree::Node& child) {
  auto parent = child.parent().MakeReferenceNode();
  // Check that the parent is indeed a gpio-controller that we support.
  if (!is_match(parent.properties())) {
    return zx::ok();
  }

  auto& controller = GetController(*parent.phandle());
  auto gpios = child.properties().find("gpios");
  if (gpios == child.properties().end()) {
    FDF_LOG(ERROR, "Gpio init hog '%s' does not have gpios property", child.name().c_str());
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  std::optional<InitCall> init_call = std::nullopt;

  if (child.properties().find("input") != child.properties().end()) {
    // Setting a temporary flag value which is updated per pin below while parsing the gpio cells.
    init_call = InitCall::WithInputFlags(static_cast<GpioFlags>(0));
  }
  if (child.properties().find("output-low") != child.properties().end()) {
    if (init_call) {
      FDF_LOG(
          ERROR,
          "Multiple values for InitCall defined in gpio init hog '%s'. Property 'output-low' clashes with another property.",
          child.name().c_str());
      return zx::error(ZX_ERR_ALREADY_EXISTS);
    }
    init_call = InitCall::WithOutputValue(0);
  }

  if (child.properties().find("output-high") != child.properties().end()) {
    if (init_call) {
      FDF_LOG(
          ERROR,
          "Multiple values for InitCall defined in gpio init hog '%s'. Property 'output-high' clashes with another property.",
          child.name().c_str());
      return zx::error(ZX_ERR_ALREADY_EXISTS);
    }
    init_call = InitCall::WithOutputValue(1);
  }

  // TODO(b/314693127): Provide a way to express alternate function and drive strength in
  // devicetree. One option is to consider pinctrl.

  if (!init_call) {
    FDF_LOG(ERROR, "Gpio init hog '%s' does not have a init call", child.name().c_str());
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  auto cell_size_property = parent.properties().find("#gpio-cells");
  if (cell_size_property == parent.properties().end()) {
    FDF_LOG(ERROR, "Gpio controller '%s' does not have '#gpio-cells' property",
            parent.name().c_str());
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  auto gpio_cell_size = cell_size_property->second.AsUint32();
  if (!gpio_cell_size) {
    FDF_LOG(ERROR, "Gpio controller '%s' has invalid '#gpio-cells' property",
            parent.name().c_str());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  auto gpios_bytes = gpios->second.AsBytes();
  size_t entry_size = (*gpio_cell_size) * sizeof(uint32_t);

  if (gpios_bytes.size_bytes() % entry_size != 0) {
    FDF_LOG(
        ERROR,
        "Gpio init hog '%s' has incorrect number of gpio cells (%lu) - expected multiple of %d cells.",
        child.name().c_str(), gpios_bytes.size_bytes() / sizeof(uint32_t), *gpio_cell_size);
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  for (size_t byte_idx = 0; byte_idx < gpios_bytes.size_bytes(); byte_idx += entry_size) {
    auto gpio = GpioCells(gpios->second.AsBytes().subspan(byte_idx, entry_size));

    // Update flags for InputFlag type.
    if (init_call->Which() == InitCall::Tag::kInputFlags) {
      zx::result flags = gpio.flags();
      if (flags.is_error()) {
        FDF_LOG(ERROR, "Failed to get input flags for gpio init hog '%s' with gpio pin %d : %s",
                child.name().c_str(), gpio.pin(), flags.status_string());
        return flags.take_error();
      }
      init_call = InitCall::WithInputFlags(*flags);
    }

    FDF_LOG(DEBUG, "Gpio init step (pin 0x%x) added to controller '%s'", gpio.pin(),
            parent.name().c_str());

    fuchsia_hardware_gpioimpl::InitStep step = {{gpio.pin(), *init_call}};
    controller.init_steps.steps().emplace_back(step);
  }

  return zx::ok();
}

GpioImplVisitor::GpioController& GpioImplVisitor::GetController(fdf_devicetree::Phandle phandle) {
  auto controller_iter = gpio_controllers_.find(phandle);
  if (controller_iter == gpio_controllers_.end()) {
    gpio_controllers_[phandle] = GpioController();
  }
  return gpio_controllers_[phandle];
}

zx::result<> GpioImplVisitor::ParseReferenceChild(fdf_devicetree::Node& child,
                                                  fdf_devicetree::ReferenceNode& parent,
                                                  fdf_devicetree::PropertyCells specifiers,
                                                  std::optional<std::string_view> gpio_name) {
  if (!gpio_name) {
    FDF_LOG(ERROR, "Gpio reference '%s' does not have a name", child.name().c_str());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  auto reference_name = std::string(*gpio_name);

  auto& controller = GetController(*parent.phandle());

  if (specifiers.size_bytes() != 2 * sizeof(uint32_t)) {
    FDF_LOG(ERROR,
            "Gpio reference '%s' has incorrect number of gpio specifiers (%lu) - expected 2.",
            child.name().c_str(), specifiers.size_bytes() / sizeof(uint32_t));
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  auto cells = GpioCells(specifiers);
  gpio_pin_t pin;
  pin.pin = cells.pin();
  strncpy(pin.name, reference_name.c_str(), sizeof(pin.name));

  FDF_LOG(DEBUG, "Gpio pin added - pin 0x%x name '%s' to controller '%s'", cells.pin(),
          reference_name.c_str(), parent.name().c_str());

  controller.gpio_pins_metadata.insert(controller.gpio_pins_metadata.end(),
                                       reinterpret_cast<const uint8_t*>(&pin),
                                       reinterpret_cast<const uint8_t*>(&pin) + sizeof(gpio_pin_t));

  return AddChildNodeSpec(child, pin.pin, reference_name);
}

zx::result<> GpioImplVisitor::FinalizeNode(fdf_devicetree::Node& node) {
  // Check that it is indeed a gpio-controller that we support.
  if (!is_match(node.properties())) {
    return zx::ok();
  }

  if (node.phandle()) {
    auto controller = gpio_controllers_.find(*node.phandle());
    if (controller == gpio_controllers_.end()) {
      FDF_LOG(INFO, "Gpio controller '%s' is not being used. Not adding any metadata for it.",
              node.name().c_str());
      return zx::ok();
    }

    {
      fuchsia_hardware_gpioimpl::ControllerMetadata metadata = {{.id = *node.phandle()}};
      const fit::result encoded_controller_metadata = fidl::Persist(metadata);
      if (!encoded_controller_metadata.is_ok()) {
        FDF_LOG(ERROR, "Failed to encode GPIO controller metadata for node %s: %s",
                node.name().c_str(),
                encoded_controller_metadata.error_value().FormatDescription().c_str());
        return zx::error(encoded_controller_metadata.error_value().status());
      }
      fuchsia_hardware_platform_bus::Metadata controller_metadata = {{
          .type = DEVICE_METADATA_GPIO_CONTROLLER,
          .data = encoded_controller_metadata.value(),
      }};
      node.AddMetadata(std::move(controller_metadata));
      FDF_LOG(DEBUG, "Gpio controller metadata added to node '%s'", node.name().c_str());
    }

    if (!controller->second.init_steps.steps().empty()) {
      const fit::result encoded_init_steps = fidl::Persist(controller->second.init_steps);
      if (!encoded_init_steps.is_ok()) {
        FDF_LOG(ERROR, "Failed to encode GPIO init metadata for node %s: %s", node.name().c_str(),
                encoded_init_steps.error_value().FormatDescription().c_str());
        return zx::error(encoded_init_steps.error_value().status());
      }

      fuchsia_hardware_platform_bus::Metadata init_metadata = {{
          .type = DEVICE_METADATA_GPIO_INIT,
          .data = encoded_init_steps.value(),
      }};
      node.AddMetadata(std::move(init_metadata));
      FDF_LOG(DEBUG, "Gpio init steps metadata added to node '%s'", node.name().c_str());
    }

    if (!controller->second.gpio_pins_metadata.empty()) {
      fuchsia_hardware_platform_bus::Metadata pin_metadata = {{
          .type = DEVICE_METADATA_GPIO_PINS,
          .data = controller->second.gpio_pins_metadata,
      }};
      node.AddMetadata(std::move(pin_metadata));
      FDF_LOG(DEBUG, "Gpio pins metadata added to node '%s'", node.name().c_str());
    }
  }

  return zx::ok();
}

}  // namespace gpio_impl_dt

REGISTER_DEVICETREE_VISITOR(gpio_impl_dt::GpioImplVisitor);
