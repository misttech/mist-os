// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_VISITORS_BTI_H_
#define LIB_DRIVER_DEVICETREE_VISITORS_BTI_H_

#include <vector>

#include "sdk/lib/driver/devicetree/visitor.h"

namespace fdf_devicetree {

// The |BtiVisitor| populates the BTI properties of each device tree
// node based on the "iommus" property if present.
class BtiVisitor : public Visitor {
 public:
  ~BtiVisitor() override = default;
  zx::result<> Visit(Node& node, const devicetree::PropertyDecoder& decoder) override;

 private:
  std::vector<std::string> iommu_nodes;
};

}  // namespace fdf_devicetree

#endif  // LIB_DRIVER_DEVICETREE_VISITORS_BTI_H_
