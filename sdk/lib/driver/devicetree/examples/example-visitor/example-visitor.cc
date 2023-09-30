// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "example-visitor.h"

#include <lib/driver/devicetree/visitors/registration.h>

namespace example {

zx::result<> ExampleDriverVisitor::DriverVisit(fdf_devicetree::Node& node,
                                               const devicetree::PropertyDecoder& decoder) {
  return zx::ok();
}

}  // namespace example

REGISTER_DEVICETREE_VISITOR(example::ExampleDriverVisitor);
