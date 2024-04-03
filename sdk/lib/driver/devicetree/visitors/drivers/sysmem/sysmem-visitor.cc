// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sysmem-visitor.h"

#include <fidl/fuchsia.hardware.sysmem/cpp/wire.h>
#include <lib/driver/devicetree/visitors/registration.h>
#include <lib/driver/logging/cpp/logger.h>

namespace sysmem_dt {

zx::result<> SysmemVisitor::DriverVisit(fdf_devicetree::Node& node,
                                        const devicetree::PropertyDecoder& decoder) {
  auto vid = node.properties().find("vid");
  if (vid == node.properties().end()) {
    FDF_LOG(ERROR, "vid not found in '%s'", node.name().data());
    return zx::ok();
  }

  auto pid = node.properties().find("pid");
  if (pid == node.properties().end()) {
    FDF_LOG(ERROR, "pid not found in '%s'", node.name().data());
    return zx::ok();
  }

  auto size = node.properties().find("size");
  if (size == node.properties().end()) {
    FDF_LOG(ERROR, "size not found in '%s'", node.name().data());
    return zx::ok();
  }

  fuchsia_hardware_sysmem::wire::Metadata sysmem_metadata = {};
  sysmem_metadata.vid = vid->second.AsUint32().value_or(0);
  sysmem_metadata.pid = pid->second.AsUint32().value_or(0);

  if (node.name() == "fuchsia,contiguous") {
    sysmem_metadata.contiguous_memory_size =
        static_cast<int64_t>(size->second.AsUint64().value_or(0));
  }

  auto serialized = std::vector<uint8_t>(
      reinterpret_cast<const uint8_t*>(&sysmem_metadata),
      reinterpret_cast<const uint8_t*>(&sysmem_metadata) + sizeof(sysmem_metadata));

  fuchsia_hardware_platform_bus::Metadata metadata = {{
      .type = fuchsia_hardware_sysmem::wire::kMetadataType,
      .data = std::move(serialized),
  }};

  node.AddMetadata(metadata);

  return zx::ok();
}

}  // namespace sysmem_dt

REGISTER_DEVICETREE_VISITOR(sysmem_dt::SysmemVisitor);
