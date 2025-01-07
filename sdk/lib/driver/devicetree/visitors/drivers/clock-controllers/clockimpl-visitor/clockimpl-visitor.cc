// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "clockimpl-visitor.h"

#include <fidl/fuchsia.hardware.clockimpl/cpp/fidl.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/visitors/registration.h>
#include <lib/driver/logging/cpp/logger.h>
#include <zircon/assert.h>
#include <zircon/errors.h>

#include <cstdint>
#include <memory>
#include <set>
#include <utility>

#include <bind/fuchsia/clock/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/clock/cpp/bind.h>
#include <ddk/metadata/clock.h>

namespace clock_impl_dt {

namespace {
using fuchsia_hardware_clockimpl::InitCall;
using fuchsia_hardware_clockimpl::InitStep;

class ClockCells {
 public:
  explicit ClockCells(fdf_devicetree::PropertyCells cells) : clock_cells_(cells, 1) {}

  // 1st cell denotes the clock ID.
  uint32_t id() { return static_cast<uint32_t>(*clock_cells_[0][0]); }

 private:
  using ClockElement = devicetree::PropEncodedArrayElement<1>;
  devicetree::PropEncodedArray<ClockElement> clock_cells_;
};

}  // namespace

ClockImplVisitor::ClockImplVisitor() {
  fdf_devicetree::Properties properties = {};
  properties.emplace_back(std::make_unique<fdf_devicetree::StringListProperty>(kClockNames));
  properties.emplace_back(
      std::make_unique<fdf_devicetree::ReferenceProperty>(kClockReference, kClockCells));
  properties.emplace_back(
      std::make_unique<fdf_devicetree::ReferenceProperty>(kAssignedClocks, kClockCells));
  properties.emplace_back(
      std::make_unique<fdf_devicetree::ReferenceProperty>(kAssignedClockParents, kClockCells));
  properties.emplace_back(
      std::make_unique<fdf_devicetree::Uint32ArrayProperty>(kAssignedClockRates));
  clock_parser_ = std::make_unique<fdf_devicetree::PropertyParser>(std::move(properties));
}

bool ClockImplVisitor::is_match(std::string_view name) {
  return name.find("clock-controller") != std::string::npos;
}

zx::result<> ClockImplVisitor::Visit(fdf_devicetree::Node& node,
                                     const devicetree::PropertyDecoder& decoder) {
  zx::result parser_output = clock_parser_->Parse(node);
  if (parser_output.is_error()) {
    FDF_LOG(ERROR, "Clock visitor failed for node '%s' : %s", node.name().c_str(),
            parser_output.status_string());
    return parser_output.take_error();
  }

  // Parse clocks and clock-names
  if (parser_output->find(kClockReference) != parser_output->end()) {
    if (parser_output->find(kClockNames) == parser_output->end() &&
        (*parser_output)[kClockReference].size() != 1u) {
      FDF_LOG(
          ERROR,
          "Clock reference '%s' does not have valid clock names property. Name is required to generate bind rules, especially when more than one clock is referenced.",
          node.name().c_str());
      return zx::error(ZX_ERR_INVALID_ARGS);
    }

    size_t count = (*parser_output)[kClockReference].size();
    std::vector<std::optional<std::string>> clock_names(count);
    if (parser_output->find(kClockNames) != parser_output->end()) {
      size_t name_idx = 0;
      for (auto& names : (*parser_output)[kClockNames]) {
        clock_names[name_idx++] = names.AsString();
      }
    }

    for (uint32_t index = 0; index < count; index++) {
      auto reference = (*parser_output)[kClockReference][index].AsReference();
      if (reference && is_match(reference->first.name())) {
        auto result =
            ParseReferenceChild(node, reference->first, reference->second, clock_names[index]);
        if (result.is_error()) {
          return result.take_error();
        }
      }
    }
  }

  // Parse assigned-clocks and related properties.
  if (parser_output->find(kAssignedClocks) != parser_output->end()) {
    size_t count = (*parser_output)[kAssignedClocks].size();

    std::vector<std::optional<fdf_devicetree::PropertyValue>> clock_parents(count);
    if (parser_output->find(kAssignedClockParents) != parser_output->end()) {
      if ((*parser_output)[kAssignedClockParents].size() >
          (*parser_output)[kAssignedClocks].size()) {
        FDF_LOG(ERROR, "Assigned clock parents in '%s' has more entries than assigned clocks.",
                node.name().c_str());
        return zx::error(ZX_ERR_INVALID_ARGS);
      }

      size_t index = 0;
      for (auto& parent : (*parser_output)[kAssignedClockParents]) {
        clock_parents[index++] = parent;
      }
    }

    std::vector<std::optional<fdf_devicetree::PropertyValue>> clock_rates(count);
    if (parser_output->find(kAssignedClockRates) != parser_output->end()) {
      if ((*parser_output)[kAssignedClockRates].size() > (*parser_output)[kAssignedClocks].size()) {
        FDF_LOG(ERROR, "Assigned clock rates in '%s' has more entries than assigned clocks.",
                node.name().c_str());
        return zx::error(ZX_ERR_INVALID_ARGS);
      }

      size_t index = 0;
      for (auto& rate : (*parser_output)[kAssignedClockRates]) {
        clock_rates[index++] = rate;
      }
    }

    // Track the clock controllers referenced so that we can add bind rule only once per controller.
    std::set<uint32_t> init_controllers;
    for (uint32_t index = 0; index < count; index++) {
      auto reference = (*parser_output)[kAssignedClocks][index].AsReference();
      if (reference && is_match(reference->first.name())) {
        auto result = ParseInitChild(node, reference->first, reference->second, clock_rates[index],
                                     clock_parents[index]);
        if (result.is_error()) {
          return result.take_error();
        }

        if (init_controllers.find(reference->first.id()) == init_controllers.end()) {
          result = AddInitChildNodeSpec(node);
          if (result.is_error()) {
            return result.take_error();
          }
          init_controllers.insert(reference->first.id());
        }
      }
    }
  }

  return zx::ok();
}

zx::result<> ClockImplVisitor::AddChildNodeSpec(fdf_devicetree::Node& child, uint32_t id,
                                                std::optional<std::string_view> clock_name) {
  auto clock_node = fuchsia_driver_framework::ParentSpec{{
      .bind_rules =
          {
              fdf::MakeAcceptBindRule(bind_fuchsia_hardware_clock::SERVICE,
                                      bind_fuchsia_hardware_clock::SERVICE_ZIRCONTRANSPORT),
              fdf::MakeAcceptBindRule(bind_fuchsia::CLOCK_ID, id),
          },
      .properties =
          {
              fdf::MakeProperty(bind_fuchsia_hardware_clock::SERVICE,
                                bind_fuchsia_hardware_clock::SERVICE_ZIRCONTRANSPORT),
          },
  }};

  if (clock_name) {
    clock_node.properties().push_back(fdf::MakeProperty(
        bind_fuchsia_clock::FUNCTION, "fuchsia.clock.FUNCTION." + std::string(*clock_name)));
  }

  child.AddNodeSpec(clock_node);
  return zx::ok();
}

zx::result<> ClockImplVisitor::AddInitChildNodeSpec(fdf_devicetree::Node& child) {
  auto clock_init_node = fuchsia_driver_framework::ParentSpec{{
      .bind_rules = {fdf::MakeAcceptBindRule(bind_fuchsia::INIT_STEP,
                                             bind_fuchsia_clock::BIND_INIT_STEP_CLOCK)},
      .properties =
          {
              fdf::MakeProperty(bind_fuchsia::INIT_STEP, bind_fuchsia_clock::BIND_INIT_STEP_CLOCK),
          },
  }};
  child.AddNodeSpec(clock_init_node);
  return zx::ok();
}

ClockImplVisitor::ClockController& ClockImplVisitor::GetController(
    fdf_devicetree::Phandle phandle) {
  auto controller_iter = clock_controllers_.find(phandle);
  if (controller_iter == clock_controllers_.end()) {
    clock_controllers_[phandle] = ClockController();
  }
  return clock_controllers_[phandle];
}

zx::result<> ClockImplVisitor::ParseReferenceChild(fdf_devicetree::Node& child,
                                                   fdf_devicetree::ReferenceNode& parent,
                                                   fdf_devicetree::PropertyCells specifiers,
                                                   std::optional<std::string_view> clock_name) {
  auto& controller = GetController(*parent.phandle());

  if (specifiers.size_bytes() != 1 * sizeof(uint32_t)) {
    FDF_LOG(ERROR,
            "Clock reference '%s' has incorrect number of clock specifiers (%lu) - expected 1.",
            child.name().c_str(), specifiers.size_bytes() / sizeof(uint32_t));
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  auto cells = ClockCells(specifiers);
  uint32_t clock_id = cells.id();

  FDF_LOG(DEBUG, "Clock ID added - ID 0x%x name '%s' to controller '%s'", cells.id(),
          clock_name ? std::string(*clock_name).c_str() : "<anonymous>", parent.name().c_str());

  auto& clock_ids = controller.clock_ids_metadata.clock_ids();
  if (!clock_ids.has_value()) {
    clock_ids.emplace(std::vector<uint32_t>{});
  }
  clock_ids.value().emplace_back(clock_id);

  return AddChildNodeSpec(child, clock_id, clock_name);
}

zx::result<> ClockImplVisitor::ParseInitChild(
    fdf_devicetree::Node& child, fdf_devicetree::ReferenceNode& parent,
    fdf_devicetree::PropertyCells specifiers,
    std::optional<fdf_devicetree::PropertyValue> clock_rate,
    std::optional<fdf_devicetree::PropertyValue> clock_parent) {
  auto& controller = GetController(*parent.phandle());
  auto clock = ClockCells(specifiers);

  if ((clock_rate && clock_rate->AsUint32().value()) || clock_parent) {
    controller.init_metadata.steps().push_back({{clock.id(), InitCall::WithDisable({})}});
  }

  if (clock_parent) {
    if (!clock_parent->AsReference()) {
      FDF_LOG(ERROR, "Assigned clock parent in '%s' is invalid", child.name().c_str());
      return zx::error(ZX_ERR_INVALID_ARGS);
    }

    auto parent_clock = ClockCells(clock_parent->AsReference()->second);
    controller.init_metadata.steps().push_back(
        {{clock.id(), InitCall::WithInputIdx(parent_clock.id())}});
    FDF_LOG(DEBUG, "Clock parent set to %d for clock ID %d by '%s'.", parent_clock.id(), clock.id(),
            child.name().c_str());
  }

  if (clock_rate) {
    if (!clock_rate->AsUint32()) {
      FDF_LOG(ERROR, "Assigned clock rate in '%s' is invalid", child.name().c_str());
      return zx::error(ZX_ERR_INVALID_ARGS);
    }

    // Skip setting rates for 0 as per the clock bindings.
    if (clock_rate->AsUint32().value() != 0) {
      controller.init_metadata.steps().push_back(
          {{clock.id(), InitCall::WithRateHz(clock_rate->AsUint32().value())}});

      FDF_LOG(DEBUG, "Clock initial rate set to %d for clock ID %d by '%s'.",
              clock_rate->AsUint32().value(), clock.id(), child.name().c_str());
    }
  }

  controller.init_metadata.steps().push_back({{clock.id(), InitCall::WithEnable({})}});

  return zx::ok();
}

zx::result<> ClockImplVisitor::FinalizeNode(fdf_devicetree::Node& node) {
  // Check that it is indeed a clock-controller that we support.
  if (!is_match(node.name())) {
    return zx::ok();
  }

  if (node.phandle()) {
    auto controller = clock_controllers_.find(*node.phandle());
    if (controller == clock_controllers_.end()) {
      FDF_LOG(INFO, "Clock controller '%s' is not being used. Not adding any metadata for it.",
              node.name().c_str());
      return zx::ok();
    }

    if (!controller->second.init_metadata.steps().empty()) {
      const fit::result encoded_metadata = fidl::Persist(controller->second.init_metadata);
      if (!encoded_metadata.is_ok()) {
        FDF_LOG(ERROR, "Failed to encode clock init metadata: %s",
                encoded_metadata.error_value().FormatDescription().c_str());
        return zx::error(encoded_metadata.error_value().status());
      }

      fuchsia_hardware_platform_bus::Metadata metadata = {{
          .id = std::to_string(DEVICE_METADATA_CLOCK_INIT),
          .data = encoded_metadata.value(),
      }};
      node.AddMetadata(std::move(metadata));
      FDF_LOG(DEBUG, "Clock init steps metadata added to node '%s'", node.name().c_str());
    }

#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
    const auto& clock_ids = controller->second.clock_ids_metadata.clock_ids();
    if (clock_ids.has_value() && !clock_ids.value().empty()) {
      const fit::result encoded_clock_id_metadata =
          fidl::Persist(controller->second.clock_ids_metadata);
      if (!encoded_clock_id_metadata.is_ok()) {
        FDF_LOG(ERROR, "Failed to encode clock ID's: %s",
                encoded_clock_id_metadata.error_value().FormatDescription().c_str());
        return zx::error(encoded_clock_id_metadata.error_value().status());
      }
      fuchsia_hardware_platform_bus::Metadata metadata = {{
          .id = fuchsia_hardware_clockimpl::wire::ClockIdsMetadata::kSerializableName,
          .data = encoded_clock_id_metadata.value(),
      }};
      node.AddMetadata(std::move(metadata));

      FDF_LOG(DEBUG, "Clock ID's metadata added to node '%s'", node.name().c_str());
    }
#endif
  }

  return zx::ok();
}

}  // namespace clock_impl_dt

REGISTER_DEVICETREE_VISITOR(clock_impl_dt::ClockImplVisitor);
