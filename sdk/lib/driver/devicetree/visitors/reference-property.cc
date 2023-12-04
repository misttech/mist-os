// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/devicetree/visitors/common-types.h>
#include <lib/driver/devicetree/visitors/reference-property.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/driver/logging/cpp/structured_logger.h>

#include <optional>

namespace fdf_devicetree {

zx::result<> ReferencePropertyParser::Visit(Node& node,
                                            const devicetree::PropertyDecoder& decoder) {
  auto reference_property = node.properties().find(reference_property_);

  if (reference_property != node.properties().end()) {
    std::optional<devicetree::StringList<>> reference_names;
    if (names_property_) {
      auto names_property = node.properties().find(*names_property_);
      if (names_property != node.properties().end()) {
        reference_names = names_property->second.AsStringList();
      } else {
        FDF_LOG(DEBUG, "Node '%s' does not have reference names property '%.*s'",
                node.name().c_str(), static_cast<int>(names_property_->length()),
                names_property_->data());
      }
    }
    std::optional reference_names_iter =
        reference_names ? std::make_optional(reference_names->begin()) : std::nullopt;

    auto cells = Uint32Array(reference_property->second.AsBytes());

    for (size_t cell_idx = 0; cell_idx < cells.size();) {
      auto phandle = cells[cell_idx];
      auto reference = node.GetReferenceNode(phandle);
      if (reference.is_error()) {
        FDF_LOG(ERROR, "Node '%s' has invalid reference in '%.*s' property to %d.",
                node.name().c_str(), static_cast<int>(reference_property_.length()),
                reference_property_.data(), phandle);
        return zx::error(ZX_ERR_INVALID_ARGS);
      }
      // Advance past phandle index.
      cell_idx++;

      auto cell_specifier = reference->properties().find(cell_specifier_);
      if (cell_specifier == reference->properties().end()) {
        FDF_LOG(ERROR, "Reference node '%s' does not have '%.*s' property.",
                reference->name().c_str(), static_cast<int>(cell_specifier_.length()),
                cell_specifier_.data());
        return zx::error(ZX_ERR_INVALID_ARGS);
      }
      auto cell_width = cell_specifier->second.AsUint32();
      if (!cell_width) {
        FDF_LOG(ERROR, "Reference node '%s' has invalid '%.*s' property.",
                reference->name().c_str(), static_cast<int>(cell_specifier_.length()),
                cell_specifier_.data());

        return zx::error(ZX_ERR_INVALID_ARGS);
      }
      size_t byteview_offset = cell_idx * sizeof(uint32_t);
      cell_idx += (*cell_width);

      if (!reference_node_matcher_(*reference)) {
        // Reference doesn't match the current parser.
        continue;
      }

      PropertyCells reference_cells = reference_property->second.AsBytes().subspan(
          byteview_offset, (*cell_width) * sizeof(uint32_t));

      std::optional<std::string> reference_name;
      if (reference_names_iter) {
        if (reference_names_iter == reference_names->end()) {
          FDF_LOG(ERROR, "Reference child node '%s' has too few reference property names '%.*s'.",
                  node.name().c_str(), static_cast<int>(names_property_->length()),
                  names_property_->data());
          return zx::error(ZX_ERR_INVALID_ARGS);
        }
        reference_name = **reference_names_iter;
        (*reference_names_iter)++;
      }

      if (reference_child_callback_) {
        zx::result result =
            reference_child_callback_(node, *reference, reference_cells, reference_name);
        if (result.is_error()) {
          FDF_LOG(ERROR,
                  "Reference child callback failed for node '%s' and reference node '%s': %s.",
                  node.name().c_str(), reference->name().c_str(), result.status_string());
          return result.take_error();
        }
      }
    }

    if (reference_names_iter && reference_names_iter != reference_names->end()) {
      FDF_LOG(ERROR, "Reference child node '%s' has too many reference property names '%.*s'.",
              node.name().c_str(), static_cast<int>(names_property_->length()),
              names_property_->data());
      return zx::error(ZX_ERR_INVALID_ARGS);
    }
  }
  return zx::ok();
}

}  // namespace fdf_devicetree
