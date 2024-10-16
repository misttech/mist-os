// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_VISITORS_DEFAULT_MMIO_MMIO_H_
#define LIB_DRIVER_DEVICETREE_VISITORS_DEFAULT_MMIO_MMIO_H_

#include <lib/driver/devicetree/manager/visitor.h>

namespace fdf_devicetree {

// The |MmioVisitor| populates the mmio properties of each device tree
// node based on the "reg" property.
class MmioVisitor : public Visitor {
 public:
  ~MmioVisitor() override = default;
  zx::result<> Visit(Node& node, const devicetree::PropertyDecoder& decoder) override;
  zx::result<> FinalizeNode(Node& node) override;

 private:
  std::map<NodeID, std::vector<fuchsia_hardware_platform_bus::Mmio>> node_mmios_;
};

}  // namespace fdf_devicetree

#endif  // LIB_DRIVER_DEVICETREE_VISITORS_DEFAULT_MMIO_MMIO_H_
