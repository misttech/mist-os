// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/devicetree/visitors/common-types.h>
#include <lib/driver/devicetree/visitors/property-parser.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/driver/logging/cpp/structured_logger.h>

#include <cstdint>
#include <optional>

namespace fdf_devicetree {

zx::result<PropertyValues> PropertyParser::Parse(Node& node) {
  PropertyValues all_values;

  for (auto& property : properties_) {
    auto raw_value = node.properties().find(property->name());
    if (raw_value != node.properties().end()) {
      auto property_values = property->Parse(node, raw_value->second.AsBytes());
      if (property_values.is_error()) {
        FDF_LOG(ERROR, "Node '%s' has an invalid property '%s'", node.name().c_str(),
                property->name().c_str());
        return property_values.take_error();
      }
      all_values[property->name()] = std::move(*property_values);
    } else {
      if (property->required()) {
        FDF_LOG(ERROR, "Node '%s' does not include the required property '%s'",
                node.name().c_str(), property->name().c_str());
        return zx::error(ZX_ERR_NOT_FOUND);
      }
    }
  }
  return zx::ok(std::move(all_values));
}

zx::result<std::vector<PropertyValue>> Property::Parse(Node& node,
                                                       devicetree::ByteView bytes) const {
  std::vector<PropertyValue> value = {PropertyValue(bytes)};
  return zx::ok(std::move(value));
}

zx::result<std::vector<PropertyValue>> Uint32ArrayProperty::Parse(
    Node& node, devicetree::ByteView bytes) const {
  if (bytes.size() % sizeof(uint32_t) != 0) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  std::vector<PropertyValue> values = {};
  for (size_t i = 0; i < bytes.size(); i += sizeof(uint32_t)) {
    auto entry = bytes.subspan(i, sizeof(uint32_t));
    values.emplace_back(entry);
  }
  return zx::ok(std::move(values));
}

zx::result<std::vector<PropertyValue>> StringListProperty::Parse(Node& node,
                                                                 devicetree::ByteView bytes) const {
  auto list = devicetree::PropertyValue(bytes).AsStringList();
  if (!list) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  std::vector<PropertyValue> values = {};
  uint32_t offset = 0;
  for (auto entry : *list) {
    size_t length = entry.length() + 1;
    devicetree::ByteView entry_bytes = bytes.subspan(offset, length);
    offset += length;
    values.emplace_back(entry_bytes);
  }
  return zx::ok(std::move(values));
}

zx::result<std::vector<PropertyValue>> ReferenceProperty::Parse(Node& node,
                                                                devicetree::ByteView bytes) const {
  if (bytes.size() % sizeof(uint32_t) != 0) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  auto cells = Uint32Array(bytes);

  std::vector<PropertyValue> values = {};
  for (size_t cell_offset = 0; cell_offset < cells.size();) {
    auto phandle = cells[cell_offset];
    auto reference = node.GetReferenceNode(phandle);
    if (reference.is_error()) {
      FDF_LOG(ERROR, "Node '%s' has invalid reference in '%s' property to %d.", node.name().c_str(),
              name().c_str(), phandle);
      return zx::error(ZX_ERR_INVALID_ARGS);
    }

    // Advance past phandle.
    cell_offset++;
    uint32_t cell_count = 0;
    if (std::holds_alternative<PropertyName>(cell_specifier_)) {
      auto cells_prop_name = std::get<PropertyName>(cell_specifier_);
      auto cell_specifier = reference->properties().find(cells_prop_name);
      if (cell_specifier == reference->properties().end()) {
        FDF_LOG(ERROR, "Reference node '%s' does not have '%s' property.",
                reference->name().c_str(), cells_prop_name.c_str());
        return zx::error(ZX_ERR_INVALID_ARGS);
      }
      auto cell_specifier_value = cell_specifier->second.AsUint32();
      if (!cell_specifier_value) {
        FDF_LOG(ERROR, "Reference node '%s' has invalid '%s' property.", reference->name().c_str(),
                cells_prop_name.c_str());

        return zx::error(ZX_ERR_INVALID_ARGS);
      }
      cell_count = *cell_specifier_value;
    } else {
      cell_count = std::get<uint32_t>(cell_specifier_);
    }

    size_t width_in_bytes = cell_count * sizeof(uint32_t);
    size_t byteview_offset = cell_offset * sizeof(uint32_t);
    cell_offset += cell_count;

    if (byteview_offset > bytes.size() || (width_in_bytes > bytes.size() - byteview_offset)) {
      FDF_LOG(
          ERROR,
          "Reference node '%s' has less data than expected. Expected %zu bytes, remaining %zu bytes",
          reference->name().c_str(), width_in_bytes, bytes.size() - byteview_offset);
      return zx::error(ZX_ERR_INVALID_ARGS);
    }

    PropertyCells reference_cells = bytes.subspan(byteview_offset, width_in_bytes);

    values.emplace_back(reference_cells, *reference);
  }

  return zx::ok(std::move(values));
}

std::optional<std::pair<ReferenceNode, PropertyCells>> PropertyValue::AsReference() const {
  if (!parent_) {
    return std::nullopt;
  }
  return {{*parent_, devicetree::PropertyValue::AsBytes()}};
}

}  // namespace fdf_devicetree
