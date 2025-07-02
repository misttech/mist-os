// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "interconnect-visitor.h"

#include <fidl/fuchsia.hardware.interconnect/cpp/fidl.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/visitors/common-types.h>
#include <lib/driver/devicetree/visitors/property-parser.h>
#include <lib/driver/devicetree/visitors/registration.h>
#include <lib/driver/logging/cpp/logger.h>
#include <zircon/assert.h>
#include <zircon/errors.h>

#include <cstdint>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/interconnect/cpp/bind.h>
#include <bind/fuchsia/interconnect/cpp/bind.h>

namespace interconnect_dt {

namespace {

class InterconnectCells {
 public:
  explicit InterconnectCells(fdf_devicetree::PropertyCells cells)
      : interconnect_cells_(cells, 1, 1, 1) {}

  uint32_t src_interconnect_phandle() const { return (uint32_t)interconnect_cells_[0][1].value(); }
  uint32_t src_node_id() const { return (uint32_t)interconnect_cells_[0][0].value(); }
  uint32_t dst_interconnect_phandle() const { return (uint32_t)interconnect_cells_[0][1].value(); }
  uint32_t dst_node_id() const { return (uint32_t)interconnect_cells_[0][2].value(); }

 private:
  using InterconnectElement = devicetree::PropEncodedArrayElement<3>;
  devicetree::PropEncodedArray<InterconnectElement> interconnect_cells_;
};

// This class attempts to parse the following devicetree structure:
// <src_phandle src_id dst_phandle dst_id>
class InterconnectReferenceProperty : public fdf_devicetree::Property {
 public:
  explicit InterconnectReferenceProperty(fdf_devicetree::PropertyName name)
      : Property(std::move(name), false) {}

  zx::result<std::vector<fdf_devicetree::PropertyValue>> Parse(
      fdf_devicetree::Node& node, devicetree::ByteView bytes) const override {
    if (bytes.size() % sizeof(uint32_t) != 0) {
      return zx::error(ZX_ERR_INVALID_ARGS);
    }

    auto cells = fdf_devicetree::Uint32Array(bytes);

    std::vector<fdf_devicetree::PropertyValue> values;
    for (size_t cell_offset = 0; cell_offset < cells.size();) {
      auto phandle = cells[cell_offset];
      zx::result reference = node.GetReferenceNode(phandle);
      if (reference.is_error()) {
        fdf::error("Node '{}' has invalid reference in '{}' property to {}.", node.name(), name(),
                   phandle);
        return zx::error(ZX_ERR_INVALID_ARGS);
      }

      // Advance past phandle.
      cell_offset++;
      uint32_t cell_count = 0;
      constexpr char kInterconnectCells[] = "#interconnect-cells";
      if (!reference->properties().contains(kInterconnectCells)) {
        fdf::error("Reference node '{}' does not have '{}' property.", reference->name(),
                   kInterconnectCells);
        return zx::error(ZX_ERR_INVALID_ARGS);
      }
      std::optional cell_count_prop = reference->properties().at(kInterconnectCells).AsUint32();
      if (!cell_count_prop) {
        fdf::error("Reference node '{}' has invalid '{}' property.", reference->name(),
                   kInterconnectCells);

        return zx::error(ZX_ERR_INVALID_ARGS);
      }
      cell_count = *cell_count_prop;

      // Each tuple contains 3 values: (src_node_id, src_interconnect_phandle, dst_node_id).
      // The node ids are cell count and we have 2 of those, whereas the phandle is 1 cell. Each
      // cell is a uint32_t.
      size_t width_in_bytes = cell_count * 2 * sizeof(uint32_t) + 4;
      size_t byteview_offset = cell_offset * sizeof(uint32_t);
      cell_offset += cell_count;

      if (byteview_offset > bytes.size() || (width_in_bytes > bytes.size() - byteview_offset)) {
        fdf::error(
            "Reference node '{}' has less data than expected. Expected {} bytes, remaining {} bytes",
            reference->name(), width_in_bytes, bytes.size() - byteview_offset);
        return zx::error(ZX_ERR_INVALID_ARGS);
      }

      auto phandle2 = cells[cell_offset];
      if (phandle2 != phandle) {
        fdf::error("Node '{}' has invalid reference in '{}' property to {}.", node.name(), name(),
                   phandle2);
        return zx::error(ZX_ERR_INVALID_ARGS);
      }
      cell_offset++;
      cell_offset += cell_count;

      fdf_devicetree::PropertyCells reference_cells =
          bytes.subspan(byteview_offset, width_in_bytes);

      values.emplace_back(reference_cells, *reference);
    }

    return zx::ok(std::move(values));
  }
};

fdf_devicetree::Properties MakeProperties() {
  fdf_devicetree::Properties props;
  props.emplace_back(std::make_unique<fdf_devicetree::StringListProperty>(
      InterconnectVisitor::kInterconnectNames));
  props.emplace_back(
      std::make_unique<InterconnectReferenceProperty>(InterconnectVisitor::kInterconnectReference));
  return props;
}

}  // namespace

InterconnectVisitor::InterconnectVisitor() : parser_(MakeProperties()) {}

bool InterconnectVisitor::IsMatch(std::string_view name) {
  return name.starts_with("interconnect");
}

zx::result<> InterconnectVisitor::Visit(fdf_devicetree::Node& node,
                                        const devicetree::PropertyDecoder& decoder) {
  zx::result parse_result = parser_.Parse(node);
  if (parse_result.is_error()) {
    fdf::error("Interconnect visitor failed for node '{}' : {}", node.name(), parse_result);
    return parse_result.take_error();
  }

  const auto& node_properties = parse_result.value();

  // Parse interconnects and interconnect-names
  if (!node_properties.contains(kInterconnectReference)) {
    return zx::ok();
  }

  fdf::debug("Found node with interconnect reference: {}", node.name());

  if (!node_properties.contains(kInterconnectNames)) {
    fdf::error(
        "Interconnect reference '{}' does not have valid interconnect names property."
        "Name is required to generate bind rules.",
        node.name());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  const size_t count = node_properties.at(kInterconnectReference).size();
  if (node_properties.at(kInterconnectNames).size() != count) {
    fdf::error(
        "Interconnect reference '{}' does not have valid number of interconnect names."
        "{} interconnects found, and {} interconnect names found, they must be equal.",
        node.name(), count, node_properties.at(kInterconnectNames).size());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  std::vector<std::string> path_names(count);
  size_t name_idx = 0;
  for (const auto& name : node_properties.at(kInterconnectNames)) {
    if (const auto& name_str = name.AsString(); name_str.has_value()) {
      path_names[name_idx++] = name_str.value();
    } else {
      fdf::error("Interconnect reference '{}' has invalid interconnect name at index {}.",
                 node.name(), name_idx);
      return zx::error(ZX_ERR_INVALID_ARGS);
    }
  }

  for (size_t index = 0; index < count; index++) {
    const auto& reference = node_properties.at(kInterconnectReference)[index].AsReference();
    const auto& [parent, cells] = reference.value();
    if (IsMatch(parent.name())) {
      zx::result result = ParseReferenceChild(node, parent, cells, path_names[index]);
      if (result.is_error()) {
        return result.take_error();
      }
    }
  }

  return zx::ok();
}

zx::result<> InterconnectVisitor::AddChildNodeSpec(fdf_devicetree::Node& child, uint32_t id,
                                                   std::string_view path_name) {
  auto interconnect_node = fuchsia_driver_framework::ParentSpec2{{
      .bind_rules =
          {
              fdf::MakeAcceptBindRule2(
                  bind_fuchsia_hardware_interconnect::PATHSERVICE,
                  bind_fuchsia_hardware_interconnect::PATHSERVICE_ZIRCONTRANSPORT),
              fdf::MakeAcceptBindRule2(bind_fuchsia::INTERCONNECT_PATH_ID, id),
          },
      .properties =
          {
              fdf::MakeProperty2(bind_fuchsia_interconnect::PATH_NAME, std::string(path_name)),
              fdf::MakeProperty2(bind_fuchsia_hardware_interconnect::PATHSERVICE,
                                 bind_fuchsia_hardware_interconnect::PATHSERVICE_ZIRCONTRANSPORT),
          },
  }};

  child.AddNodeSpec(interconnect_node);
  return zx::ok();
}

InterconnectVisitor::Interconnect& InterconnectVisitor::GetInterconnect(
    fdf_devicetree::Phandle phandle) {
  if (!interconnects_.contains(phandle)) {
    interconnects_[phandle] = Interconnect{};
  }
  return interconnects_[phandle];
}

zx::result<> InterconnectVisitor::ParseReferenceChild(fdf_devicetree::Node& child,
                                                      const fdf_devicetree::ReferenceNode& parent,
                                                      fdf_devicetree::PropertyCells specifiers,
                                                      std::string_view path_name) {
  fdf::debug("Parsing reference child: {}", child.name());
  auto& interconnect = GetInterconnect(parent.phandle().value());

  if (specifiers.size_bytes() != 3 * sizeof(uint32_t)) {
    fdf::error(
        "Interconnect reference '{}' has incorrect number of interconnect specifiers ({}) - expected 3.",
        child.name(), specifiers.size_bytes() / sizeof(uint32_t));
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  InterconnectCells prop(specifiers);
  fuchsia_hardware_interconnect::PathEndpoints path{{
      .name = std::string(path_name),
      .id = id_++,
      .src_node_id = prop.src_node_id(),
      .dst_node_id = prop.dst_node_id(),
  }};

  fdf::debug("Interconnect ID added - ID 0x{:x} name '{}' to interconnect '{}'", *path.id(),
             path_name, parent.name());

  auto& paths = interconnect.metadata.paths();
  if (!paths.has_value()) {
    paths.emplace(std::vector<fuchsia_hardware_interconnect::PathEndpoints>{});
  }
  paths->emplace_back(path);

  return AddChildNodeSpec(child, path.id().value(), path_name);
}

zx::result<> InterconnectVisitor::FinalizeNode(fdf_devicetree::Node& node) {
  // Check that it is indeed a interconnect that we support.
  if (!IsMatch(node.name())) {
    return zx::ok();
  }

  if (!node.phandle()) {
    return zx::ok();
  }

  if (!interconnects_.contains(*node.phandle())) {
    fdf::debug("Interconnect '{}' is not being used. Not adding any metadata for it.", node.name());
    return zx::ok();
  }
  const Interconnect& interconnect = interconnects_.at(*node.phandle());

  const auto& paths = interconnect.metadata.paths();
  if (paths.has_value() && !paths.value().empty()) {
    const fit::result encoded_metadata = fidl::Persist(interconnect.metadata);
    if (!encoded_metadata.is_ok()) {
      fdf::error("Failed to encode interconnect paths: {}", encoded_metadata.error_value());
      return zx::error(encoded_metadata.error_value().status());
    }
    node.AddMetadata(fuchsia_hardware_platform_bus::Metadata{{
        .id = fuchsia_hardware_interconnect::Metadata::kSerializableName,
        .data = encoded_metadata.value(),
    }});

    fdf::debug("Interconnect node ID's metadata added to node '{}'", node.name());
  }

  return zx::ok();
}

}  // namespace interconnect_dt

REGISTER_DEVICETREE_VISITOR(interconnect_dt::InterconnectVisitor);
