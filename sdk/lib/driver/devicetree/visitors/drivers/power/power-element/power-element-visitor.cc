// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "power-element-visitor.h"

#include <lib/driver/devicetree/visitors/registration.h>
#include <lib/driver/logging/cpp/logger.h>

#include <algorithm>
#include <optional>
#include <regex>
#include <string>
#include <vector>

namespace {
using fuchsia_hardware_power::CpuPowerElement;
using fuchsia_hardware_power::LevelTuple;
using fuchsia_hardware_power::ParentElement;
using fuchsia_hardware_power::PowerDependency;
using fuchsia_hardware_power::PowerElement;
using fuchsia_hardware_power::PowerElementConfiguration;
using fuchsia_hardware_power::PowerLevel;
using fuchsia_hardware_power::RequirementType;
using fuchsia_hardware_power::SagElement;
using fuchsia_hardware_power::Transition;
}  // namespace

namespace power_element_visitor_dt {

// 1 cell to represent the type
constexpr const uint32_t kPowerDependencyCellSize = 1;
class PowerDependencyCell {
 public:
  using PowerDependencyArray = devicetree::PropEncodedArrayElement<kPowerDependencyCellSize>;

  explicit PowerDependencyCell(fdf_devicetree::PropertyCells cells)
      : property_array_(cells, kPowerDependencyCellSize) {}

  // 1st cell represents the type.
  RequirementType type() { return static_cast<RequirementType>(*property_array_[0][0]); }

 private:
  devicetree::PropEncodedArray<PowerDependencyArray> property_array_;
};

PowerElementVisitor::PowerElementVisitor() {
  fdf_devicetree::Properties level_properties = {};
  level_properties.emplace_back(std::make_unique<fdf_devicetree::Uint32Property>(kLevel, true));
  level_properties.emplace_back(
      std::make_unique<fdf_devicetree::ReferenceProperty>(kLevelDependencies, 1u));
  level_parser_ = std::make_unique<fdf_devicetree::PropertyParser>(std::move(level_properties));
  fdf_devicetree::Properties transition_properties = {};
  transition_properties.emplace_back(
      std::make_unique<fdf_devicetree::Uint32Property>(kTargetLevel, true));
  transition_properties.emplace_back(std::make_unique<fdf_devicetree::Uint32Property>(kLatencyUs));
  transition_parser_ =
      std::make_unique<fdf_devicetree::PropertyParser>(std::move(transition_properties));
}

bool PowerElementVisitor::IsMatch(fdf_devicetree::Node& node) {
  return node.parent() && node.name() == "power-elements";
}

std::optional<std::string> PowerElementVisitor::GetElementName(const std::string& node_name) {
  std::smatch match;
  std::regex name_regex("(^[a-zA-Z0-9-]*)-element$");
  if (std::regex_search(node_name, match, name_regex) && match.size() == 2) {
    return match[1];
  }
  return std::nullopt;
}

zx::result<> PowerElementVisitor::Visit(fdf_devicetree::Node& node,
                                        const devicetree::PropertyDecoder& decoder) {
  if (!IsMatch(node)) {
    return zx::ok();
  }

  // Do not add System Activity Governor power elements.
  if (node.parent().name() == "system-activity-governor") {
    return zx::ok();
  }

  auto device_node = node.parent().GetNode();

  for (auto& child : node.children()) {
    auto element_config = ParsePowerElement(*child.GetNode());
    if (element_config.is_error()) {
      return element_config.take_error();
    }
    FDF_LOG(DEBUG, "Added power element '%s' to node '%s'.",
            element_config->element()->name()->c_str(), device_node->name().c_str());
    device_node->AddPowerConfig(*element_config);
  }

  return zx::ok();
}

zx::result<PowerElementConfiguration> PowerElementVisitor::ParsePowerElement(
    fdf_devicetree::Node& node) {
  PowerElementConfiguration element_config;
  element_config.element() = PowerElement();
  element_config.element()->name() = GetElementName(node.name());

  if (!element_config.element()->name()) {
    FDF_LOG(ERROR, "Power element has invalid node name '%s'.", node.name().c_str());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  auto children = node.children();
  if (children.size() != 1u || children[0].name() != "power-levels") {
    FDF_LOG(
        ERROR,
        "Power element has invalid child nodes '%s'. Expecting a single child node named 'power-levels'.",
        node.name().c_str());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  element_config.element()->levels() = std::vector<PowerLevel>();

  // Parse the power-level nodes.
  auto power_level_nodes = children[0].GetNode()->children();
  for (auto& child : power_level_nodes) {
    zx::result result = ParseLevel(*child.GetNode(), element_config);
    if (result.is_error()) {
      return result.take_error();
    }
  }
  return zx::ok(element_config);
}

std::optional<std::string> PowerElementVisitor::GetLevelName(const std::string& node_name) {
  std::smatch match;
  std::regex name_regex("(^[a-zA-Z0-9-]*)-level$");
  if (std::regex_search(node_name, match, name_regex) && match.size() == 2) {
    return match[1];
  }
  return std::nullopt;
}

std::optional<ParentElement> PowerElementVisitor::GetParentElementFromLevelRef(
    fdf_devicetree::ReferenceNode& level_in_parent) {
  // Reference node points to a specific power level node. The parent will be power-levels node.
  // It's parent will be the power element node.
  if (!level_in_parent.parent() /*power-levels node*/ ||
      !level_in_parent.parent().parent() /*power element x*/ ||
      !level_in_parent.parent().parent().parent() /*power-elements node*/ ||
      !level_in_parent.parent().parent().parent().parent() /* device node*/) {
    FDF_LOG(ERROR, "Power level reference node '%s' should be under a power-element node.",
            level_in_parent.name().c_str());
    return std::nullopt;
  }

  // |level_in_parent| points to a x-element/power-levels/x-level. Get the pointer to the power
  // element.
  auto power_element = level_in_parent.parent().parent();
  // |power_element| points to a device-x/power-elements/x-element. Get the pointer to the device
  // node.
  auto device_node = power_element.parent().parent();

  if (device_node.name() == "system-activity-governor") {
    if (power_element.name() == "execution-state-element") {
      return ParentElement::WithSag(SagElement::kExecutionState);
    }
    if (power_element.name() == "application-activity-element") {
      return ParentElement::WithSag(SagElement::kApplicationActivity);
    }
    FDF_LOG(ERROR, "Power level reference node '%s' is an invalid SAG element '%s'.",
            level_in_parent.name().c_str(), power_element.name().c_str());
    return std::nullopt;
  }

  if (device_node.name() == "cpu") {
    if (power_element.name() == "cpu-element") {
      return ParentElement::WithCpuControl(CpuPowerElement::kCpu);
    }
  }

  auto parent_name = GetElementName(power_element.name());
  if (!parent_name) {
    FDF_LOG(ERROR, "Power level reference node '%s' has an invalid element name '%s'.",
            level_in_parent.name().c_str(), power_element.name().c_str());
    return std::nullopt;
  }

  return ParentElement::WithInstanceName(*parent_name);
}

PowerDependency& PowerElementVisitor::GetPowerDependency(PowerElementConfiguration& element_config,
                                                         const std::string& child_name,
                                                         const ParentElement& parent,
                                                         const RequirementType& type) {
  if (!element_config.dependencies()) {
    element_config.dependencies() = std::vector<PowerDependency>();
  }

  // Check if the dependency already exists.
  for (auto& dependency : *element_config.dependencies()) {
    if (dependency.child() == child_name && dependency.parent() == parent &&
        dependency.strength() == type) {
      return dependency;
    }
  }

  PowerDependency dependency;
  dependency.child() = child_name;
  dependency.parent() = parent;
  dependency.strength() = type;
  dependency.level_deps() = std::vector<LevelTuple>();
  element_config.dependencies()->push_back(dependency);
  return element_config.dependencies()->back();
}

zx::result<> PowerElementVisitor::ParseLevel(fdf_devicetree::Node& node,
                                             PowerElementConfiguration& element_config) {
  PowerLevel level;
  level.name() = GetLevelName(node.name());

  if (!level.name()) {
    FDF_LOG(ERROR, "Power element has invalid node name '%s'.", node.name().c_str());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  zx::result parser_output = level_parser_->Parse(node);
  if (parser_output.is_error()) {
    FDF_LOG(ERROR, "Power level parse failed for node '%s' : %s", node.name().c_str(),
            parser_output.status_string());
    return parser_output.take_error();
  }

  level.level() = parser_output->at(kLevel)[0].AsUint32();

  // Parse level dependencies if exists.
  if (parser_output->find(kLevelDependencies) != parser_output->end()) {
    for (auto& entry : parser_output->at(kLevelDependencies)) {
      auto parent_level_ref = entry.AsReference()->first;
      auto parent_element = GetParentElementFromLevelRef(parent_level_ref);
      if (!parent_element) {
        return zx::error(ZX_ERR_INVALID_ARGS);
      }

      auto type = PowerDependencyCell(entry.AsReference()->second).type();

      PowerDependency& dependency = GetPowerDependency(
          element_config, *element_config.element()->name(), *parent_element, type);

      LevelTuple level_tuple;
      level_tuple.child_level() = level.level();

      if (parent_level_ref.properties().find(kLevel) == parent_level_ref.properties().end()) {
        FDF_LOG(ERROR, "Power level reference node '%s' has no level property.",
                parent_level_ref.name().c_str());
        return zx::error(ZX_ERR_INVALID_ARGS);
      }
      level_tuple.parent_level() = parent_level_ref.properties().at(kLevel).AsUint32();

      dependency.level_deps()->push_back(level_tuple);
    }
  }

  // Parse transition table if exists.
  if (!node.children().empty()) {
    if (node.children().size() != 1u || node.children()[0].name() != "level-transition-table") {
      FDF_LOG(
          ERROR,
          "Power level has invalid child nodes '%s'. Expecting a single child node named 'level-transition-table'.",
          node.name().c_str());
      return zx::error(ZX_ERR_INVALID_ARGS);
    }

    zx::result result = ParseTransitionTable(*node.children()[0].GetNode(), level);
    if (result.is_error()) {
      return result.take_error();
    }
  }

  element_config.element()->levels()->push_back(level);
  return zx::ok();
}

zx::result<> PowerElementVisitor::ParseTransitionTable(fdf_devicetree::Node& node,
                                                       PowerLevel& level) {
  level.transitions() = std::vector<Transition>();
  for (auto& child : node.children()) {
    auto parser_output = transition_parser_->Parse(*child.GetNode());
    if (parser_output.is_error()) {
      FDF_LOG(ERROR, "Failed to parse transition entry '%s'", child.name().c_str());
      return parser_output.take_error();
    }
    Transition transition;
    transition.target_level() = parser_output->at(kTargetLevel)[0].AsUint32();
    if (parser_output->find(kLatencyUs) != parser_output->end()) {
      transition.latency_us() = parser_output->at(kLatencyUs)[0].AsUint32();
    }
    level.transitions()->push_back(transition);
  }
  return zx::ok();
}

}  // namespace power_element_visitor_dt

REGISTER_DEVICETREE_VISITOR(power_element_visitor_dt::PowerElementVisitor);
