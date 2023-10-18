// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/v1/composite_node_spec_v1.h"

#include "src/devices/lib/log/log.h"

namespace fdd = fuchsia_driver_development;

namespace composite_node_specs {

zx::result<std::unique_ptr<CompositeNodeSpecV1>> CompositeNodeSpecV1::Create(
    CompositeNodeSpecCreateInfo create_info,
    fuchsia_device_manager::wire::CompositeNodeSpecDescriptor spec, DeviceManager& device_manager) {
  fbl::Array<std::unique_ptr<Metadata>> metadata(
      new std::unique_ptr<Metadata>[spec.metadata.count()], spec.metadata.count());
  for (size_t i = 0; i < spec.metadata.count(); i++) {
    std::unique_ptr<Metadata> md;
    auto status = Metadata::Create(spec.metadata[i].data.count(), &md);
    if (status != ZX_OK) {
      LOGF(ERROR, "Failed to create metadata %s", zx_status_get_string(status));
      return zx::error(status);
    }

    md->type = spec.metadata[i].key;
    md->length = static_cast<uint32_t>(spec.metadata[i].data.count());
    memcpy(md->Data(), spec.metadata[i].data.data(), md->length);
    metadata[i] = std::move(md);
  }

  return zx::ok(std::make_unique<CompositeNodeSpecV1>(std::move(create_info), std::move(metadata),
                                                      device_manager));
}

CompositeNodeSpecV1::CompositeNodeSpecV1(CompositeNodeSpecCreateInfo create_info,
                                         fbl::Array<std::unique_ptr<Metadata>> metadata,
                                         DeviceManager& device_manager)
    : CompositeNodeSpec(std::move(create_info)),
      metadata_(std::move(metadata)),
      has_composite_device_(false),
      device_manager_(device_manager) {}

zx::result<std::optional<DeviceOrNode>> CompositeNodeSpecV1::BindParentImpl(
    fuchsia_driver_framework::wire::CompositeParent composite_parent,
    const DeviceOrNode& device_or_node) {
  auto device_ptr = std::get_if<std::weak_ptr<DeviceV1Wrapper>>(&device_or_node);
  ZX_ASSERT(device_ptr);
  auto owned = device_ptr->lock();
  if (!owned) {
    LOGF(ERROR, "DeviceV1Wrapper weak_ptr not available");
    return zx::error(ZX_ERR_INTERNAL);
  }

  if (!has_composite_device_) {
    SetupCompositeDevice(composite_parent);
  }

  auto owned_device = owned->device;
  auto result = device_manager_.BindFragmentForSpec(owned_device, name(), composite_parent.index());
  if (result.is_error()) {
    LOGF(ERROR, "Failed to BindFragment for '%.*s': %s",
         static_cast<uint32_t>(owned_device->name().size()), owned_device->name().data(),
         result.status_string());
    return result.take_error();
  }

  LOGF(DEBUG, "Node '%s' matched composite node spec '%s' with parent spec '%s'",
       owned_device->name().c_str(), name().c_str(),
       parent_names_[composite_parent.index()].c_str());

  return zx::ok(std::nullopt);
}

void CompositeNodeSpecV1::SetupCompositeDevice(
    fuchsia_driver_framework::wire::CompositeParent composite_parent) {
  ZX_ASSERT(!has_composite_device_);
  ZX_ASSERT(composite_parent.has_composite());
  auto composite_info = fidl::ToNatural(composite_parent.composite());
  device_manager_.AddCompositeDeviceFromSpec(composite_info, std::move(metadata_));
  has_composite_device_ = true;
  metadata_ = fbl::Array<std::unique_ptr<Metadata>>();
  composite_info_ = std::move(composite_info);

  auto& matched_driver = composite_info_.matched_driver();
  ZX_ASSERT(matched_driver.has_value());
  auto& parent_names = matched_driver.value().parent_names();
  ZX_ASSERT(parent_names.has_value());
  parent_names_ = parent_names.value();
}

fdd::wire::CompositeNodeInfo CompositeNodeSpecV1::GetCompositeInfo(fidl::AnyArena& arena) const {
  fidl::VectorView<fidl::StringView> parent_topological_paths(arena, size());
  fidl::StringView topological_path;
  if (has_composite_device_) {
    auto topological_info = device_manager_.GetTopologicalInfo(arena, name());
    if (topological_info.is_ok()) {
      topological_path = topological_info->path;
      parent_topological_paths = topological_info->parent_paths;
    }
  } else {
    // Create parent_topological_paths with all null elements.
    topological_path = fidl::StringView();
    for (auto i = 0u; i < size(); i++) {
      parent_topological_paths[i] = fidl::StringView();
    }
  }

  auto composite_info = fdd::wire::CompositeNodeInfo::Builder(arena)
                            .composite(fdd::wire::CompositeInfo::WithComposite(
                                arena, fidl::ToWire(arena, composite_info_)))
                            .parent_topological_paths(parent_topological_paths)
                            .topological_path(topological_path)
                            .Build();
  return composite_info;
}

void CompositeNodeSpecV1::RemoveImpl(RemoveCompositeNodeCallback callback) {
  // TODO(fxb/124976): Implement this.
  callback(zx::error(ZX_ERR_NOT_SUPPORTED));
}

}  // namespace composite_node_specs
