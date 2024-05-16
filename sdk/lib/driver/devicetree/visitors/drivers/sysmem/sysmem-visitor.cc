// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sysmem-visitor.h"

#include <fidl/fuchsia.hardware.sysmem/cpp/fidl.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/visitors/registration.h>
#include <lib/driver/logging/cpp/logger.h>

#include <bind/fuchsia/hardware/sysmem/cpp/bind.h>

#include "lib/driver/devicetree/visitors/property-parser.h"

namespace sysmem_dt {

SysmemVisitor::SysmemVisitor() : DriverVisitor({"fuchsia,sysmem"}) {
  fdf_devicetree::Properties properties = {};
  properties.emplace_back(std::make_unique<fdf_devicetree::Uint32Property>(kVid));
  properties.emplace_back(std::make_unique<fdf_devicetree::Uint32Property>(kPid));
  properties.emplace_back(std::make_unique<fdf_devicetree::Uint64Property>(kContiguousSize));
  properties.emplace_back(std::make_unique<fdf_devicetree::Uint64Property>(kProtectedSize));
  parser_ = std::make_unique<fdf_devicetree::PropertyParser>(std::move(properties));
}

zx::result<> SysmemVisitor::DriverVisit(fdf_devicetree::Node& node,
                                        const devicetree::PropertyDecoder& decoder) {
  auto parser_output = parser_->Parse(node);
  if (parser_output.is_error()) {
    FDF_LOG(ERROR, "Sysmem visitor failed for node '%s' : %s", node.name().c_str(),
            parser_output.status_string());
    return parser_output.take_error();
  }

  fuchsia_hardware_sysmem::Metadata sysmem_metadata;
  if (parser_output->find(kVid) != parser_output->end()) {
    sysmem_metadata.vid() = parser_output->at(kVid)[0].AsUint32();
  }
  if (parser_output->find(kPid) != parser_output->end()) {
    sysmem_metadata.pid() = parser_output->at(kPid)[0].AsUint32();
  }

  if (parser_output->find(kContiguousSize) != parser_output->end()) {
    sysmem_metadata.contiguous_memory_size() = parser_output->at(kContiguousSize)[0].AsUint64();
  }

  if (parser_output->find(kProtectedSize) != parser_output->end()) {
    sysmem_metadata.protected_memory_size() = parser_output->at(kProtectedSize)[0].AsUint64();
  }

  auto serialized_result = fidl::Persist(sysmem_metadata);
  ZX_ASSERT(serialized_result.is_ok());
  auto serialized = std::move(serialized_result.value());

  fuchsia_hardware_platform_bus::Metadata metadata = {{
      .type = fuchsia_hardware_sysmem::wire::kMetadataType,
      .data = std::move(serialized),
  }};

  node.AddMetadata(metadata);
  FDF_LOG(DEBUG, "Added sysmem metadata vid: %d pid: %d contiguous size: %ld protected size: %ld.",
          sysmem_metadata.vid().value_or(0), sysmem_metadata.pid().value_or(0),
          sysmem_metadata.contiguous_memory_size().value_or(0),
          sysmem_metadata.protected_memory_size().value_or(0));

  return zx::ok();
}

zx::result<> SysmemVisitor::AddChildNodeSpec(fdf_devicetree::Node& child) {
  std::vector bind_rules = {
      fdf::MakeAcceptBindRule(bind_fuchsia_hardware_sysmem::SERVICE,
                              bind_fuchsia_hardware_sysmem::SERVICE_ZIRCONTRANSPORT)};

  std::vector bind_properties = {
      fdf::MakeProperty(bind_fuchsia_hardware_sysmem::SERVICE,
                        bind_fuchsia_hardware_sysmem::SERVICE_ZIRCONTRANSPORT)};

  auto sysmem_node = fuchsia_driver_framework::ParentSpec{{bind_rules, bind_properties}};

  child.AddNodeSpec(sysmem_node);
  FDF_LOG(DEBUG, "Added sysmem node spec to '%s'.", child.name().c_str());

  return zx::ok();
}

zx::result<> SysmemVisitor::Visit(fdf_devicetree::Node& node,
                                  const devicetree::PropertyDecoder& decoder) {
  if (node.properties().find(kSysmemReference) != node.properties().end()) {
    return AddChildNodeSpec(node);
  }

  if (is_match(node.properties())) {
    return DriverVisit(node, decoder);
  }

  return zx::ok();
}

}  // namespace sysmem_dt

REGISTER_DEVICETREE_VISITOR(sysmem_dt::SysmemVisitor);
