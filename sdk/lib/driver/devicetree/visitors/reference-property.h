// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_VISITORS_REFERENCE_PROPERTY_H_
#define LIB_DRIVER_DEVICETREE_VISITORS_REFERENCE_PROPERTY_H_

#include <lib/driver/devicetree/manager/visitor.h>
#include <lib/fit/function.h>

#include <optional>

namespace fdf_devicetree {

using PropertyName = std::string_view;
// Cells specific to the property represented in a prop-encoded-array.
using PropertyCells = devicetree::ByteView;

class ReferencePropertyParser {
 public:
  using ReferenceNodeMatchCallback = fit::function<bool(ReferenceNode&)>;
  using ReferenceChildCallback =
      fit::function<zx::result<>(Node& child, ReferenceNode& parent, PropertyCells specifiers,
                                 std::optional<std::string> reference_name)>;

  explicit ReferencePropertyParser(PropertyName reference_property, PropertyName cell_specifier,
                                   std::optional<PropertyName> names_property,
                                   ReferenceNodeMatchCallback reference_node_matcher,
                                   ReferenceChildCallback reference_child_callback = nullptr)
      : reference_property_(reference_property),
        cell_specifier_(cell_specifier),
        names_property_(names_property),
        reference_node_matcher_(std::move(reference_node_matcher)),
        reference_child_callback_(std::move(reference_child_callback)) {}

  virtual ~ReferencePropertyParser() = default;

  virtual zx::result<> Visit(Node& node, const devicetree::PropertyDecoder& decoder);

 private:
  // Property holding the reference to other nodes. Eg: clocks.
  PropertyName reference_property_;
  // Property specifying reference cell width. Eg: #clock-cells.
  PropertyName cell_specifier_;
  // Associated name for the reference if any. Eg: clock-names.
  std::optional<PropertyName> names_property_;
  ReferenceNodeMatchCallback reference_node_matcher_;
  ReferenceChildCallback reference_child_callback_;
};

}  // namespace fdf_devicetree

#endif  // LIB_DRIVER_DEVICETREE_VISITORS_REFERENCE_PROPERTY_H_
