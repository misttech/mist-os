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
#include <utility>

#include <bind/fuchsia/clock/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <ddk/metadata/clock.h>

namespace clock_impl_dt {

namespace {

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

  if (parser_output->find(kClockReference) == parser_output->end()) {
    return zx::ok();
  }

  if (parser_output->find(kClockNames) == parser_output->end() ||
      (*parser_output)[kClockReference].size() != (*parser_output)[kClockReference].size()) {
    // We need a clock names to generate bind rules.
    FDF_LOG(ERROR, "Clock reference '%s' does not have valid clock names property.",
            node.name().c_str());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  for (uint32_t index = 0; index < (*parser_output)[kClockReference].size(); index++) {
    auto reference = (*parser_output)[kClockReference][index].AsReference();
    if (reference && is_match(reference->first.name())) {
      auto result = ParseReferenceChild(node, reference->first, reference->second,
                                        (*parser_output)[kClockNames][index].AsString());
      if (result.is_error()) {
        return result.take_error();
      }
    }
  }

  return zx::ok();
}

zx::result<> ClockImplVisitor::AddChildNodeSpec(fdf_devicetree::Node& child, uint32_t id,
                                                std::string clock_name) {
  auto clock_node = fuchsia_driver_framework::ParentSpec{{
      .bind_rules =
          {
              fdf::MakeAcceptBindRule(bind_fuchsia::PROTOCOL,
                                      bind_fuchsia_clock::BIND_FIDL_PROTOCOL_SERVICE),
              fdf::MakeAcceptBindRule(bind_fuchsia::CLOCK_ID, id),
          },
      .properties =
          {
              fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL,
                                bind_fuchsia_clock::BIND_FIDL_PROTOCOL_SERVICE),
              fdf::MakeProperty(bind_fuchsia_clock::FUNCTION,
                                "fuchsia.clock.FUNCTION." + clock_name),
          },
  }};
  child.AddNodeSpec(clock_node);
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
  if (!clock_name) {
    FDF_LOG(ERROR, "Clock reference '%s' does not have a valid name", child.name().c_str());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  auto reference_name = std::string(*clock_name);

  auto& controller = GetController(*parent.phandle());

  if (specifiers.size_bytes() != 1 * sizeof(uint32_t)) {
    FDF_LOG(ERROR,
            "Clock reference '%s' has incorrect number of clock specifiers (%lu) - expected 1.",
            child.name().c_str(), specifiers.size_bytes() / sizeof(uint32_t));
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  auto cells = ClockCells(specifiers);
  clock_id_t id;
  id.clock_id = cells.id();

  FDF_LOG(DEBUG, "Clock ID added - ID 0x%x name '%s' to controller '%s'", cells.id(),
          reference_name.c_str(), parent.name().c_str());

  controller.clock_ids_metadata.insert(controller.clock_ids_metadata.end(),
                                       reinterpret_cast<const uint8_t*>(&id),
                                       reinterpret_cast<const uint8_t*>(&id) + sizeof(clock_id_t));

  return AddChildNodeSpec(child, id.clock_id, reference_name);
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

    if (!controller->second.clock_ids_metadata.empty()) {
      fuchsia_hardware_platform_bus::Metadata id_metadata = {{
          .type = DEVICE_METADATA_CLOCK_IDS,
          .data = controller->second.clock_ids_metadata,
      }};
      node.AddMetadata(std::move(id_metadata));
      FDF_LOG(DEBUG, "Clock IDs metadata added to node '%s'", node.name().c_str());
    }
  }

  return zx::ok();
}

}  // namespace clock_impl_dt

REGISTER_DEVICETREE_VISITOR(clock_impl_dt::ClockImplVisitor);
