// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_VISITORS_MMIO_H_
#define LIB_DRIVER_DEVICETREE_VISITORS_MMIO_H_

#include "sdk/lib/driver/devicetree/visitor.h"

namespace fdf_devicetree {

// The |MmioVisitor| populates the mmio properties of each device tree
// node based on the "reg" property.
class MmioVisitor : public Visitor {
 public:
  ~MmioVisitor() override = default;
  zx::result<> Visit(Node& node, const devicetree::PropertyDecoder& decoder) override;
};

}  // namespace fdf_devicetree

#endif  // LIB_DRIVER_DEVICETREE_VISITORS_MMIO_H_
