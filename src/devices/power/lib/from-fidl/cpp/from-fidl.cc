// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/power/lib/from-fidl/cpp/from-fidl.h"

namespace power::from_fidl {

zx::result<fdf_power::Transition> CreateTransition(const fuchsia_hardware_power::Transition& src) {
  const auto& target_level = src.target_level();
  if (!target_level.has_value()) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  const auto& latency_us = src.latency_us();
  if (!latency_us.has_value()) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  return zx::ok(fdf_power::Transition{.target_level = target_level.value(),
                                      .latency_us = latency_us.value()});
}

zx::result<fdf_power::Transition> CreateTransition(
    const fuchsia_hardware_power::wire::Transition& src) {
  if (!src.has_target_level() || !src.has_latency_us()) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  return zx::ok(
      fdf_power::Transition{.target_level = src.target_level(), .latency_us = src.latency_us()});
}

zx::result<fdf_power::LevelTuple> CreateLevelTuple(const fuchsia_hardware_power::LevelTuple& src) {
  const auto& child_level = src.child_level();
  if (!child_level.has_value()) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  const auto& parent_level = src.parent_level();
  if (!parent_level.has_value()) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  return zx::ok(fdf_power::LevelTuple{.child_level = child_level.value(),
                                      .parent_level = parent_level.value()});
}

zx::result<fdf_power::LevelTuple> CreateLevelTuple(
    const fuchsia_hardware_power::wire::LevelTuple& src) {
  if (!src.has_child_level() || !src.has_parent_level()) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  return zx::ok(
      fdf_power::LevelTuple{.child_level = src.child_level(), .parent_level = src.parent_level()});
}

zx::result<fdf_power::PowerLevel> CreatePowerLevel(const fuchsia_hardware_power::PowerLevel& src) {
  const auto& level = src.level();
  if (!level.has_value()) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  const auto& name = src.name();
  if (!name.has_value()) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  std::vector<fdf_power::Transition> transitions;
  const auto& fidl_transitions = src.transitions();
  if (fidl_transitions.has_value()) {
    for (const auto& fidl_transition : fidl_transitions.value()) {
      zx::result transition = CreateTransition(fidl_transition);
      if (transition.is_error()) {
        return transition.take_error();
      }
      transitions.push_back(transition.value());
    }
  }

  return zx::ok(fdf_power::PowerLevel{
      .level = level.value(), .name{name.value()}, .transitions{std::move(transitions)}});
}

zx::result<fdf_power::PowerLevel> CreatePowerLevel(
    const fuchsia_hardware_power::wire::PowerLevel& src) {
  if (!src.has_level() || !src.has_name()) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  std::vector<fdf_power::Transition> transitions;
  if (src.has_transitions()) {
    for (const auto& fidl_transition : src.transitions()) {
      zx::result transition = CreateTransition(fidl_transition);
      if (transition.is_error()) {
        return transition.take_error();
      }
      transitions.push_back(transition.value());
    }
  }

  return zx::ok(fdf_power::PowerLevel{
      .level = src.level(), .name{src.name().get()}, .transitions{std::move(transitions)}});
}

zx::result<fdf_power::PowerElement> CreatePowerElement(
    const fuchsia_hardware_power::PowerElement& src) {
  const auto& name = src.name();
  if (!name.has_value()) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  std::vector<fdf_power::PowerLevel> levels;
  const auto& fidl_levels = src.levels();
  if (fidl_levels.has_value()) {
    for (const auto& fidl_level : fidl_levels.value()) {
      zx::result level = CreatePowerLevel(fidl_level);
      if (level.is_error()) {
        return level.take_error();
      }
      levels.push_back(std::move(level.value()));
    }
  }

  return zx::ok(fdf_power::PowerElement{.name{name.value()}, .levels{std::move(levels)}});
}

zx::result<fdf_power::PowerElement> CreatePowerElement(
    const fuchsia_hardware_power::wire::PowerElement& src) {
  if (!src.has_name()) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  std::vector<fdf_power::PowerLevel> levels;
  if (src.has_levels()) {
    for (const auto& fidl_level : src.levels()) {
      zx::result level = CreatePowerLevel(fidl_level);
      if (level.is_error()) {
        return level.take_error();
      }
      levels.push_back(std::move(level.value()));
    }
  }

  return zx::ok(fdf_power::PowerElement{.name{src.name().get()}, .levels{std::move(levels)}});
}

zx::result<fdf_power::ParentElement> CreateParentElement(
    const fuchsia_hardware_power::ParentElement& src) {
  switch (src.Which()) {
    case fuchsia_hardware_power::ParentElement::Tag::kSag: {
      const auto& fidl_sag = src.sag();
      if (!fidl_sag.has_value()) {
        return zx::error(ZX_ERR_INVALID_ARGS);
      }
      fdf_power::SagElement sag;
      switch (fidl_sag.value()) {
        case fuchsia_hardware_power::SagElement::kExecutionState:
          sag = fdf_power::SagElement::kExecutionState;
          break;
        case fuchsia_hardware_power::SagElement::kExecutionResumeLatency:
          sag = fdf_power::SagElement::kExecutionResumeLatency;
          break;
        case fuchsia_hardware_power::SagElement::kWakeHandling:
          sag = fdf_power::SagElement::kWakeHandling;
          break;
        case fuchsia_hardware_power::SagElement::kApplicationActivity:
          sag = fdf_power::SagElement::kApplicationActivity;
          break;
      }
      return zx::ok(fdf_power::ParentElement::WithSag(sag));
    }
    case fuchsia_hardware_power::ParentElement::Tag::kInstanceName: {
      const auto& fidl_instance_name = src.instance_name();
      if (!fidl_instance_name.has_value()) {
        return zx::error(ZX_ERR_INVALID_ARGS);
      }
      return zx::ok(fdf_power::ParentElement::WithInstanceName(fidl_instance_name.value()));
    }
    case fuchsia_hardware_power::ParentElement::Tag::kCpuControl: {
      return zx::error(ZX_ERR_NOT_SUPPORTED);
    }
  }
}

zx::result<fdf_power::ParentElement> CreateParentElement(
    const fuchsia_hardware_power::wire::ParentElement& src) {
  switch (src.Which()) {
    case fuchsia_hardware_power::wire::ParentElement::Tag::kSag: {
      fdf_power::SagElement sag;
      switch (src.sag()) {
        case fuchsia_hardware_power::wire::SagElement::kExecutionState:
          sag = fdf_power::SagElement::kExecutionState;
          break;
        case fuchsia_hardware_power::wire::SagElement::kExecutionResumeLatency:
          sag = fdf_power::SagElement::kExecutionResumeLatency;
          break;
        case fuchsia_hardware_power::wire::SagElement::kWakeHandling:
          sag = fdf_power::SagElement::kWakeHandling;
          break;
        case fuchsia_hardware_power::wire::SagElement::kApplicationActivity:
          sag = fdf_power::SagElement::kApplicationActivity;
          break;
      }
      return zx::ok(fdf_power::ParentElement::WithSag(sag));
    }
    case fuchsia_hardware_power::wire::ParentElement::Tag::kInstanceName: {
      return zx::ok(
          fdf_power::ParentElement::WithInstanceName(std::string{src.instance_name().get()}));
    }
    case fuchsia_hardware_power::wire::ParentElement::Tag::kCpuControl: {
      return zx::error(ZX_ERR_NOT_SUPPORTED);
    }
  }
}

zx::result<fdf_power::PowerDependency> CreatePowerDependency(
    const fuchsia_hardware_power::PowerDependency& src) {
  const auto& child = src.child();
  if (!child.has_value()) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  const auto& fidl_parent = src.parent();
  if (!fidl_parent.has_value()) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  zx::result parent = CreateParentElement(fidl_parent.value());
  if (parent.is_error()) {
    return parent.take_error();
  }

  std::vector<fdf_power::LevelTuple> level_deps;
  const auto& fidl_level_deps = src.level_deps();
  if (fidl_level_deps.has_value()) {
    for (const auto& fidl_level_dep : fidl_level_deps.value()) {
      zx::result level_dep = CreateLevelTuple(fidl_level_dep);
      if (level_dep.is_error()) {
        return level_dep.take_error();
      }
      level_deps.push_back(level_dep.value());
    }
  }

  const auto& fidl_strength = src.strength();
  if (!fidl_strength.has_value()) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  fdf_power::RequirementType strength;
  switch (fidl_strength.value()) {
    case fuchsia_hardware_power::RequirementType::kAssertive:
      strength = fdf_power::RequirementType::kAssertive;
      break;
    case fuchsia_hardware_power::RequirementType::kOpportunistic:
      strength = fdf_power::RequirementType::kOpportunistic;
      break;
  }

  return zx::ok(fdf_power::PowerDependency{.child{child.value()},
                                           .parent{std::move(parent.value())},
                                           .level_deps{std::move(level_deps)},
                                           .strength = strength});
}

zx::result<fdf_power::PowerDependency> CreatePowerDependency(
    const fuchsia_hardware_power::wire::PowerDependency& src) {
  if (!src.has_child() || !src.has_parent() || !src.has_strength()) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  zx::result parent = CreateParentElement(src.parent());
  if (parent.is_error()) {
    return parent.take_error();
  }

  std::vector<fdf_power::LevelTuple> level_deps;
  if (src.has_level_deps()) {
    for (const auto& fidl_level_dep : src.level_deps()) {
      zx::result level_dep = CreateLevelTuple(fidl_level_dep);
      if (level_dep.is_error()) {
        return level_dep.take_error();
      }
      level_deps.push_back(level_dep.value());
    }
  }

  fdf_power::RequirementType strength;
  switch (src.strength()) {
    case fuchsia_hardware_power::wire::RequirementType::kAssertive:
      strength = fdf_power::RequirementType::kAssertive;
      break;
    case fuchsia_hardware_power::wire::RequirementType::kOpportunistic:
      strength = fdf_power::RequirementType::kOpportunistic;
      break;
  }

  return zx::ok(fdf_power::PowerDependency{.child{std::string{src.child().get()}},
                                           .parent{std::move(parent.value())},
                                           .level_deps{std::move(level_deps)},
                                           .strength = strength});
}

zx::result<fdf_power::PowerElementConfiguration> CreatePowerElementConfiguration(
    const fuchsia_hardware_power::PowerElementConfiguration& src) {
  const auto& fidl_element = src.element();
  if (!fidl_element.has_value()) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  zx::result element = CreatePowerElement(fidl_element.value());
  if (element.is_error()) {
    return element.take_error();
  }

  std::vector<fdf_power::PowerDependency> dependencies;
  const auto& fidl_dependencies = src.dependencies();
  if (fidl_dependencies.has_value()) {
    for (const auto& fidl_dependency : fidl_dependencies.value()) {
      zx::result dependency = CreatePowerDependency(fidl_dependency);
      if (dependency.is_error()) {
        return dependency.take_error();
      }
      dependencies.push_back(std::move(dependency.value()));
    }
  }

  return zx::ok(fdf_power::PowerElementConfiguration{.element{std::move(element.value())},
                                                     .dependencies{std::move(dependencies)}});
}

zx::result<fdf_power::PowerElementConfiguration> CreatePowerElementConfiguration(
    const fuchsia_hardware_power::wire::PowerElementConfiguration& src) {
  if (!src.has_element()) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  zx::result element = CreatePowerElement(src.element());
  if (element.is_error()) {
    return element.take_error();
  }

  std::vector<fdf_power::PowerDependency> dependencies;
  if (src.has_dependencies()) {
    for (const auto& fidl_dependency : src.dependencies()) {
      zx::result dependency = CreatePowerDependency(fidl_dependency);
      if (dependency.is_error()) {
        return dependency.take_error();
      }
      dependencies.push_back(std::move(dependency.value()));
    }
  }

  return zx::ok(fdf_power::PowerElementConfiguration{.element{std::move(element.value())},
                                                     .dependencies{std::move(dependencies)}});
}

}  // namespace power::from_fidl
