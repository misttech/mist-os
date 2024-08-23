// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sysmem-visitor.h"

#include <fidl/fuchsia.hardware.sysmem/cpp/fidl.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/visitors/registration.h>
#include <lib/driver/logging/cpp/logger.h>

#include "lib/driver/devicetree/visitors/property-parser.h"

namespace sysmem_dt {

SysmemVisitor::SysmemVisitor() : DriverVisitor({"fuchsia,sysmem"}) {}

zx::result<> SysmemVisitor::DriverVisit(fdf_devicetree::Node& node,
                                        const devicetree::PropertyDecoder& decoder) {
  // Sysmem should be published first so that it can reserve memory as needed.
  return node.ChangePublishOrder(1u);
}

zx::result<> SysmemVisitor::Visit(fdf_devicetree::Node& node,
                                  const devicetree::PropertyDecoder& decoder) {
  if (is_match(node.properties())) {
    return DriverVisit(node, decoder);
  }

  return zx::ok();
}

}  // namespace sysmem_dt

REGISTER_DEVICETREE_VISITOR(sysmem_dt::SysmemVisitor);
