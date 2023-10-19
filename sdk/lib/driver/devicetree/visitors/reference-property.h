// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_VISITORS_REFERENCE_PROPERTY_H_
#define LIB_DRIVER_DEVICETREE_VISITORS_REFERENCE_PROPERTY_H_

#include <lib/driver/devicetree/manager/visitor.h>
#include <lib/fit/function.h>

namespace fdf_devicetree {

using PropertyName = std::string_view;
// Cells specific to the property represented in a prop-encoded-array.
using PropertyCells = devicetree::ByteView;

class ReferencePropertyParser {
 public:
  using ReferenceNodeMatchCallback = fit::function<bool(ReferenceNode&)>;
  using ReferenceChildCallback =
      fit::function<zx::result<>(Node& child, ReferenceNode& parent, PropertyCells specifiers)>;

  explicit ReferencePropertyParser(PropertyName reference_property, PropertyName cell_specifier,
                                   ReferenceNodeMatchCallback reference_node_matcher,
                                   ReferenceChildCallback reference_child_callback = nullptr)
      : reference_property_(reference_property),
        cell_specifier_(cell_specifier),
        reference_node_matcher_(std::move(reference_node_matcher)),
        reference_child_callback_(std::move(reference_child_callback)) {}

  zx::result<> Visit(Node& node, const devicetree::PropertyDecoder& decoder);

 private:
  PropertyName reference_property_;
  PropertyName cell_specifier_;
  ReferenceNodeMatchCallback reference_node_matcher_;
  ReferenceChildCallback reference_child_callback_;
};

}  // namespace fdf_devicetree

#endif  // LIB_DRIVER_DEVICETREE_VISITORS_REFERENCE_PROPERTY_H_
