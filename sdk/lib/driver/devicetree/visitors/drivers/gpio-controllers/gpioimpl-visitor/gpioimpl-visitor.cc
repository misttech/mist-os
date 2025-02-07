// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "gpioimpl-visitor.h"

#include <fidl/fuchsia.hardware.pinimpl/cpp/fidl.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/visitors/common-types.h>
#include <lib/driver/devicetree/visitors/registration.h>
#include <lib/driver/logging/cpp/logger.h>
#include <zircon/assert.h>
#include <zircon/errors.h>

#include <algorithm>
#include <cstdint>
#include <memory>
#include <optional>
#include <string_view>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/gpio/cpp/bind.h>

namespace gpio_impl_dt {

namespace {
using fuchsia_hardware_gpio::BufferMode;
using fuchsia_hardware_pin::DriveType;
using fuchsia_hardware_pin::Pull;
using fuchsia_hardware_pinimpl::InitCall;
using fuchsia_hardware_pinimpl::Metadata;

class GpioCells {
 public:
  explicit GpioCells(fdf_devicetree::PropertyCells cells) : gpio_cells_(cells, 1, 1) {}

  // 1st cell denotes the gpio pin.
  uint32_t pin() { return static_cast<uint32_t>(*gpio_cells_[0][0]); }

  // 2nd cell represents GpioFlags. This is only used in gpio init hog nodes and ignored elsewhere.
  zx::result<Pull> flags() {
    switch (static_cast<uint32_t>(*gpio_cells_[0][1])) {
      case 0:
        return zx::ok(Pull::kDown);
      case 1:
        return zx::ok(Pull::kUp);
      case 2:
        return zx::ok(Pull::kNone);
      default:
        return zx::error(ZX_ERR_INVALID_ARGS);
    };
  }

 private:
  using GpioElement = devicetree::PropEncodedArrayElement<2>;
  devicetree::PropEncodedArray<GpioElement> gpio_cells_;
};

}  // namespace

// TODO(b/325077980): Name of the reference property can be *-gpios.
GpioImplVisitor::GpioImplVisitor() {
  fdf_devicetree::Properties gpio_properties = {};
  gpio_properties.emplace_back(
      std::make_unique<fdf_devicetree::ReferenceProperty>(kGpioReference, kGpioCells));
  gpio_properties.emplace_back(std::make_unique<fdf_devicetree::StringListProperty>(kGpioNames));
  gpio_parser_ = std::make_unique<fdf_devicetree::PropertyParser>(std::move(gpio_properties));

  fdf_devicetree::Properties pinctrl_state_properties = {};
  pinctrl_state_properties.emplace_back(
      std::make_unique<fdf_devicetree::ReferenceProperty>(kPinCtrl0, 0u));
  pinctrl_state_parser_ =
      std::make_unique<fdf_devicetree::PropertyParser>(std::move(pinctrl_state_properties));
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
    auto result = ParseGpioHogChild(node);
    if (result.is_error()) {
      FDF_LOG(ERROR, "Gpio visitor failed for node '%s' : %s", node.name().c_str(),
              result.status_string());
    }
  } else {
    auto gpio_props = gpio_parser_->Parse(node);

    if (gpio_props->find(kGpioReference) != gpio_props->end()) {
      if (gpio_props->find(kGpioNames) == gpio_props->end() ||
          (*gpio_props)[kGpioNames].size() != (*gpio_props)[kGpioReference].size()) {
        // We need a gpio names to generate bind rules.
        FDF_LOG(ERROR, "Gpio reference '%s' does not have valid gpio names field.",
                node.name().c_str());
        return zx::error(ZX_ERR_INVALID_ARGS);
      }

      for (uint32_t index = 0; index < (*gpio_props)[kGpioReference].size(); index++) {
        auto reference = (*gpio_props)[kGpioReference][index].AsReference();
        if (reference && is_match(reference->first.properties())) {
          auto result = ParseReferenceChild(node, reference->first, reference->second,
                                            (*gpio_props)[kGpioNames][index].AsString());
          if (result.is_error()) {
            return result.take_error();
          }
        }
      }
    }

    auto pinctrl_props = pinctrl_state_parser_->Parse(node);
    if (pinctrl_props->find(kPinCtrl0) != pinctrl_props->end()) {
      // Names of gpio controllers used in this pin control state. This is used to add gpio init
      // bind rule only once per controller.
      std::vector<uint32_t> controllers;

      uint32_t controller_index = 0;
      for (auto& pinctrl_cfg : (*pinctrl_props)[kPinCtrl0]) {
        auto reference = pinctrl_cfg.AsReference();

        auto gpio_node = GetGpioNodeForPinConfig(reference->first);
        if (gpio_node.is_error()) {
          return gpio_node.take_error();
        }

        auto result = ParsePinCtrlCfg(node, reference->first, *gpio_node);
        if (result.is_error()) {
          return result.take_error();
        }

        if (std::find(controllers.begin(), controllers.end(), gpio_node->id()) ==
            controllers.end()) {
          result = AddInitNodeSpec(node, gpio_node->id(), controller_index++);
          if (result.is_error()) {
            return result.take_error();
          }
          controllers.push_back(gpio_node->id());
        }
      }
    }
  }

  return zx::ok();
}

zx::result<> GpioImplVisitor::AddChildNodeSpec(fdf_devicetree::Node& child, uint32_t pin,
                                               uint32_t controller_id, std::string gpio_name) {
  auto gpio_node = fuchsia_driver_framework::ParentSpec{{
      .bind_rules =
          {
              fdf::MakeAcceptBindRule(bind_fuchsia_hardware_gpio::SERVICE,
                                      bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
              fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_CONTROLLER, controller_id),
              fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN, pin),
          },
      .properties =
          {
              fdf::MakeProperty(bind_fuchsia_hardware_gpio::SERVICE,
                                bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
              fdf::MakeProperty(bind_fuchsia_gpio::FUNCTION, "fuchsia.gpio.FUNCTION." + gpio_name),
          },
  }};
  child.AddNodeSpec(gpio_node);
  return zx::ok();
}

zx::result<> GpioImplVisitor::AddInitNodeSpec(fdf_devicetree::Node& child, uint32_t controller_id,
                                              uint32_t controller_index) {
  auto gpio_init_node = fuchsia_driver_framework::ParentSpec{{
      .bind_rules =
          {
              fdf::MakeAcceptBindRule(bind_fuchsia::INIT_STEP,
                                      bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
              fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_CONTROLLER, controller_id),
          },
      .properties =
          {
              fdf::MakeProperty(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
              fdf::MakeProperty(bind_fuchsia::GPIO_CONTROLLER, controller_index),
          },
  }};
  child.AddNodeSpec(gpio_init_node);
  return zx::ok();
}

zx::result<fdf_devicetree::ParentNode> GpioImplVisitor::GetGpioNodeForPinConfig(
    fdf_devicetree::ReferenceNode& cfg_node) {
  // TODO(b/325077980): Add gpio-ranges based mapping in case the pinctrl cfg is not a direct
  // child of gpio-controller.
  return zx::ok(cfg_node.parent());
}

zx::result<> GpioImplVisitor::ParsePinCtrlCfg(fdf_devicetree::Node& child,
                                              fdf_devicetree::ReferenceNode& cfg_node,
                                              fdf_devicetree::ParentNode& gpio_node) {
  // Check that the parent is indeed a gpio-controller that we support.
  if (!is_match(gpio_node.properties())) {
    return zx::ok();
  }

  auto& controller = GetController(gpio_node.id());
  auto pins_property = cfg_node.properties().find(kPins);
  if (pins_property == cfg_node.properties().end()) {
    FDF_LOG(ERROR, "Pin controller config '%s' does not have pins property",
            cfg_node.name().c_str());
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  auto pins = fdf_devicetree::Uint32Array(pins_property->second.AsBytes());
  if (pins.size() == 0) {
    FDF_LOG(ERROR, "No pins found in pin controller config '%s'", cfg_node.name().c_str());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  fuchsia_hardware_pin::Configuration config;

  std::optional<Pull> pull;
  auto save_pull = [&](Pull val) -> zx::result<> {
    if (pull.has_value()) {
      FDF_LOG(
          ERROR,
          "Pin controller config '%s' can only support one pull direction. Previously already set with %d, now trying to set as %d",
          cfg_node.name().c_str(), static_cast<uint32_t>(*pull), static_cast<uint32_t>(val));
      return zx::error(ZX_ERR_NOT_SUPPORTED);
    }
    pull = val;
    return zx::ok();
  };
  if (cfg_node.properties().find(kPinBiasPullDown) != cfg_node.properties().end()) {
    auto result = save_pull(Pull::kDown);
    if (result.is_error()) {
      return result.take_error();
    }
  }
  if (cfg_node.properties().find(kPinBiasPullUp) != cfg_node.properties().end()) {
    auto result = save_pull(Pull::kUp);
    if (result.is_error()) {
      return result.take_error();
    }
  }
  if (cfg_node.properties().find(kPinBiasDisable) != cfg_node.properties().end()) {
    auto result = save_pull(Pull::kNone);
    if (result.is_error()) {
      return result.take_error();
    }
  }

  if (pull.has_value()) {
    config.pull(*pull);
  }

  if (cfg_node.properties().find(kPinFunction) != cfg_node.properties().end()) {
    auto function = cfg_node.properties().at(kPinFunction).AsUint64();
    if (!function) {
      FDF_LOG(ERROR, "Pin controller config '%s' has invalid function.", cfg_node.name().c_str());
      return zx::error(ZX_ERR_INVALID_ARGS);
    }
    config.function(*function);
  }

  if (cfg_node.properties().find(kPinDriveStrengthUa) != cfg_node.properties().end()) {
    auto drive_strength_ua = cfg_node.properties().at(kPinDriveStrengthUa).AsUint64();
    if (!drive_strength_ua) {
      FDF_LOG(ERROR, "Pin controller config '%s' has invalid drive strength.",
              cfg_node.name().c_str());
      return zx::error(ZX_ERR_INVALID_ARGS);
    }
    config.drive_strength_ua(*drive_strength_ua);
  }

  std::optional<DriveType> drive_type;
  auto save_drive_type = [&](DriveType val) -> zx::result<> {
    if (drive_type.has_value()) {
      FDF_LOG(
          ERROR,
          "Pin controller config '%s' can only support one drive typ. Previously already set with %d, now trying to set as %d",
          cfg_node.name().c_str(), static_cast<uint32_t>(*drive_type), static_cast<uint32_t>(val));
      return zx::error(ZX_ERR_NOT_SUPPORTED);
    }
    drive_type = val;
    return zx::ok();
  };
  if (cfg_node.properties().find(kPinDrivePushPull) != cfg_node.properties().end()) {
    auto result = save_drive_type(DriveType::kPushPull);
    if (result.is_error()) {
      return result.take_error();
    }
  }
  if (cfg_node.properties().find(kPinDriveOpenDrain) != cfg_node.properties().end()) {
    auto result = save_drive_type(DriveType::kOpenDrain);
    if (result.is_error()) {
      return result.take_error();
    }
  }
  if (cfg_node.properties().find(kPinDriveOpenSource) != cfg_node.properties().end()) {
    auto result = save_drive_type(DriveType::kOpenSource);
    if (result.is_error()) {
      return result.take_error();
    }
  }
  if (drive_type.has_value()) {
    config.drive_type(*drive_type);
  }

  if (cfg_node.properties().find(kPinPowerSource) != cfg_node.properties().end()) {
    auto power_source = cfg_node.properties().at(kPinPowerSource).AsUint32();
    if (!power_source) {
      FDF_LOG(ERROR, "Pin controller config '%s' has invalid power source.",
              cfg_node.name().c_str());
      return zx::error(ZX_ERR_INVALID_ARGS);
    }
    config.power_source(*power_source);
  }

  std::optional<BufferMode> buffer_mode;
  if (cfg_node.properties().contains(kPinOutputDisable)) {
    buffer_mode = BufferMode::kInput;
  }
  if (cfg_node.properties().contains(kPinOutputLow)) {
    if (buffer_mode) {
      FDF_LOG(
          ERROR,
          "Multiple values for InitCall defined in pin config '%s'. Property 'output-low' clashes with another property.",
          cfg_node.name().c_str());
      return zx::error(ZX_ERR_ALREADY_EXISTS);
    }
    buffer_mode = BufferMode::kOutputLow;
  }
  if (cfg_node.properties().contains(kPinOutputHigh)) {
    if (buffer_mode) {
      FDF_LOG(
          ERROR,
          "Multiple values for InitCall defined in pin config '%s'. Property 'output-high' clashes with another property.",
          cfg_node.name().c_str());
      return zx::error(ZX_ERR_ALREADY_EXISTS);
    }
    buffer_mode = BufferMode::kOutputHigh;
  }

  std::vector<InitCall> init_calls;
  if (!config.IsEmpty()) {
    init_calls.emplace_back(InitCall::WithPinConfig(std::move(config)));
  }
  if (buffer_mode) {
    init_calls.emplace_back(InitCall::WithBufferMode(*buffer_mode));
  }
  if (init_calls.empty()) {
    FDF_LOG(ERROR, "Pin controller config '%s' does not have a valid config.",
            cfg_node.name().c_str());
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  // Add the init steps for all the pins in the config.
  for (size_t i = 0; i < pins.size(); i++) {
    FDF_LOG(DEBUG,
            "Gpio init steps (count: %zu) for child '%s' (pin 0x%x) added to controller '%s'",
            init_calls.size(), child.name().c_str(), pins[i], gpio_node.name().c_str());
    for (auto& init_call : init_calls) {
      auto step = fuchsia_hardware_pinimpl::InitStep::WithCall({{pins[i], init_call}});
      controller.metadata.init_steps()->emplace_back(step);
    }
  }

  return zx::ok();
}

zx::result<> GpioImplVisitor::ParseGpioHogChild(fdf_devicetree::Node& child) {
  auto parent = child.parent().MakeReferenceNode();
  // Check that the parent is indeed a gpio-controller that we support.
  if (!is_match(parent.properties())) {
    return zx::ok();
  }

  auto& controller = GetController(parent.id());
  auto gpios = child.properties().find("gpios");
  if (gpios == child.properties().end()) {
    FDF_LOG(ERROR, "Gpio init hog '%s' does not have gpios property", child.name().c_str());
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  std::optional<fuchsia_hardware_gpio::BufferMode> buffer_mode;

  if (child.properties().find("input") != child.properties().end()) {
    buffer_mode = fuchsia_hardware_gpio::BufferMode::kInput;
  }
  if (child.properties().find("output-low") != child.properties().end()) {
    if (buffer_mode) {
      FDF_LOG(
          ERROR,
          "Multiple values for InitCall defined in gpio init hog '%s'. Property 'output-low' clashes with another property.",
          child.name().c_str());
      return zx::error(ZX_ERR_ALREADY_EXISTS);
    }
    buffer_mode = fuchsia_hardware_gpio::BufferMode::kOutputLow;
  }

  if (child.properties().find("output-high") != child.properties().end()) {
    if (buffer_mode) {
      FDF_LOG(
          ERROR,
          "Multiple values for InitCall defined in gpio init hog '%s'. Property 'output-high' clashes with another property.",
          child.name().c_str());
      return zx::error(ZX_ERR_ALREADY_EXISTS);
    }
    buffer_mode = fuchsia_hardware_gpio::BufferMode::kOutputHigh;
  }

  if (!buffer_mode) {
    FDF_LOG(ERROR, "Gpio init hog '%s' does not have a buffer_mode", child.name().c_str());
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
    zx::result flags = gpio.flags();
    if (flags.is_error()) {
      FDF_LOG(ERROR, "Failed to get input flags for gpio init hog '%s' with gpio pin %d : %s",
              child.name().c_str(), gpio.pin(), flags.status_string());
      return flags.take_error();
    }

    controller.metadata.init_steps()->push_back(fuchsia_hardware_pinimpl::InitStep::WithCall({{
        .pin = gpio.pin(),
        .call = InitCall::WithPinConfig({{.pull = *gpio.flags()}}),
    }}));
    controller.metadata.init_steps()->push_back(fuchsia_hardware_pinimpl::InitStep::WithCall({{
        .pin = gpio.pin(),
        .call = InitCall::WithBufferMode(*buffer_mode),
    }}));

    FDF_LOG(DEBUG, "Gpio init step (pin 0x%x) added to controller '%s'", gpio.pin(),
            parent.name().c_str());
  }

  return zx::ok();
}

GpioImplVisitor::GpioController& GpioImplVisitor::GetController(uint32_t node_id) {
  auto controller_iter = gpio_controllers_.find(node_id);
  if (controller_iter == gpio_controllers_.end()) {
    gpio_controllers_[node_id] = GpioController();
  }
  return gpio_controllers_[node_id];
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
  auto& controller = GetController(parent.id());

  if (specifiers.size_bytes() != 2 * sizeof(uint32_t)) {
    FDF_LOG(ERROR,
            "Gpio reference '%s' has incorrect number of gpio specifiers (%lu) - expected 2.",
            child.name().c_str(), specifiers.size_bytes() / sizeof(uint32_t));
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  auto cells = GpioCells(specifiers);
  fuchsia_hardware_pinimpl::Pin pin{{
      .pin = cells.pin(),
      .name = reference_name,
  }};

  FDF_LOG(DEBUG, "Gpio pin added - pin 0x%x name '%s' to controller '%s'", cells.pin(),
          reference_name.c_str(), parent.name().c_str());

  // Insert if the pin is not already present.
  auto it = std::find_if(
      controller.metadata.pins()->begin(), controller.metadata.pins()->end(),
      [&pin](const fuchsia_hardware_pinimpl::Pin& entry) { return entry.pin() == pin.pin(); });
  if (it == controller.metadata.pins()->end()) {
    controller.metadata.pins()->push_back(pin);
  }

  return AddChildNodeSpec(child, pin.pin().value(), parent.id(), reference_name);
}

zx::result<> GpioImplVisitor::FinalizeNode(fdf_devicetree::Node& node) {
  // Check that it is indeed a gpio-controller that we support.
  if (!is_match(node.properties())) {
    return zx::ok();
  }

  auto controller = gpio_controllers_.find(node.id());
  if (controller == gpio_controllers_.end()) {
    FDF_LOG(INFO, "Gpio controller '%s' is not being used. Not adding any metadata for it.",
            node.name().c_str());
    return zx::ok();
  }

  {
    fuchsia_hardware_pinimpl::Metadata metadata = {{.controller_id = controller->first}};
    if (!controller->second.metadata.init_steps()->empty()) {
      metadata.init_steps() = *std::move(controller->second.metadata.init_steps());
    }
    if (!controller->second.metadata.pins()->empty()) {
      metadata.pins() = *std::move(controller->second.metadata.pins());
    }

    const fit::result encoded_controller_metadata = fidl::Persist(metadata);
    if (!encoded_controller_metadata.is_ok()) {
      FDF_LOG(ERROR, "Failed to encode GPIO controller metadata for node %s: %s",
              node.name().c_str(),
              encoded_controller_metadata.error_value().FormatDescription().c_str());
      return zx::error(encoded_controller_metadata.error_value().status());
    }

    // TODO(b/388305889): Remove once no longer retrieved.
    {
      fuchsia_hardware_platform_bus::Metadata controller_metadata = {{
          .id = std::to_string(DEVICE_METADATA_GPIO_CONTROLLER),
          .data = encoded_controller_metadata.value(),
      }};
      node.AddMetadata(std::move(controller_metadata));
    }

    {
      fuchsia_hardware_platform_bus::Metadata controller_metadata = {{
          .id = fuchsia_hardware_pinimpl::Metadata::kSerializableName,
          .data = std::move(encoded_controller_metadata.value()),
      }};
      node.AddMetadata(std::move(controller_metadata));
    }
    FDF_LOG(DEBUG, "Gpio metadata added to node '%s'", node.name().c_str());
  }

  return zx::ok();
}

}  // namespace gpio_impl_dt

REGISTER_DEVICETREE_VISITOR(gpio_impl_dt::GpioImplVisitor);
