// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include "sdk/lib/driver/devicetree/visitors/bti.h"

#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/devicetree/devicetree.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/driver/logging/cpp/structured_logger.h>

#include "sdk/lib/driver/devicetree/node.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}

namespace fdf_devicetree {

constexpr const char kBtiProp[] = "iommus";
constexpr const char kIommuCellsProp[] = "#iommu-cells";

// Restrict #iommu-cells value to 1.
// This is because fuchsia_hardware_platform_bus::Bti only takes a
// bti_id as a specifier which is u32. Therefore iommu specifier should be 1.
constexpr const uint32_t kFixedIommuCells = 1;

// phandles are u32.
constexpr const uint32_t kPhandleCells = 1;

// Iommu is specified as <phandle, bti_id>, where phandle is the reference to
// the iommu device.
constexpr const uint32_t kNumIommuFields = 2;

using IommuPropertyElement = devicetree::PropEncodedArrayElement<kNumIommuFields>;

zx::result<> BtiVisitor::Visit(Node& node, const devicetree::PropertyDecoder& decoder) {
  // Check if it's a iommu node. If so, check if the iommu specifier matches kFixedIommuCells.
  auto iommu_cells_prop = node.properties().find(kIommuCellsProp);
  if (iommu_cells_prop != node.properties().end()) {
    auto iommu_cells = iommu_cells_prop->second.AsUint32();
    if (!iommu_cells) {
      FDF_LOG(ERROR, "Node '%s' has invalid iommu-cells property.", node.name().data());
    } else if (*iommu_cells != kFixedIommuCells) {
      FDF_LOG(ERROR, "Node '%s' has invalid iommu-cells property. Expected '1' but it is '%d'.",
              node.name().data(), *iommu_cells);
    }
  }

  // Parse iommu property if specified for the node.
  auto property = node.properties().find(kBtiProp);
  if (property == node.properties().end()) {
    FDF_LOG(DEBUG, "Node '%s' has no iommus property.", node.name().data());
    return zx::ok();
  }

  devicetree::PropEncodedArray<IommuPropertyElement> iommus(property->second.AsBytes(),
                                                            kPhandleCells, kFixedIommuCells);

  for (uint32_t i = 0; i < iommus.size(); i++) {
    fuchsia_hardware_platform_bus::Bti bti = {{
        .iommu_index = iommus[i][0],
        .bti_id = iommus[i][1],
    }};
    FDF_LOG(DEBUG, "BTI (0x%0x, 0x%0lx) added to node '%s'.", *bti.iommu_index(), *bti.bti_id(),
            node.name().data());
    node.AddBti(bti);
  }

  return zx::ok();
}

}  // namespace fdf_devicetree
