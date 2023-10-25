// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_SYSMEM_SYSMEM_VISITOR_H_
#define LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_SYSMEM_SYSMEM_VISITOR_H_

#include <lib/driver/devicetree/visitors/driver-visitor.h>

namespace sysmem_dt {

class SysmemVisitor : public fdf_devicetree::DriverVisitor {
 public:
  SysmemVisitor() : DriverVisitor({"sysmem"}) {}
  zx::result<> DriverVisit(fdf_devicetree::Node& node,
                           const devicetree::PropertyDecoder& decoder) override;
};

}  // namespace sysmem_dt

#endif  // LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_SYSMEM_SYSMEM_VISITOR_H_
