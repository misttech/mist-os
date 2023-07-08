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
    fuchsia_driver_index::wire::MatchedCompositeNodeSpecInfo info,
    const DeviceOrNode& device_or_node) {
  auto device_ptr = std::get_if<std::weak_ptr<DeviceV1Wrapper>>(&device_or_node);
  ZX_ASSERT(device_ptr);
  auto owned = device_ptr->lock();
  if (!owned) {
    LOGF(ERROR, "DeviceV1Wrapper weak_ptr not available");
    return zx::error(ZX_ERR_INTERNAL);
  }

  if (!has_composite_device_) {
    SetupCompositeDevice(info);
  }

  auto owned_device = owned->device;
  auto result = device_manager_.BindFragmentForSpec(owned_device, name(), info.node_index());
  if (result.is_error()) {
    LOGF(ERROR, "Failed to BindFragment for '%.*s': %s",
         static_cast<uint32_t>(owned_device->name().size()), owned_device->name().data(),
         result.status_string());
    return result.take_error();
  }

  if (owned_device->name() == "sysmem-fidl" || owned_device->name() == "sysmem-banjo") {
    LOGF(DEBUG, "Node '%s' matched composite node spec '%s' with parent spec '%s'",
         owned_device->name().c_str(), std::string(info.name().get()).c_str(),
         parent_names_[info.node_index()].c_str());
  } else {
    LOGF(INFO, "Node '%s' matched composite node spec '%s' with parent spec '%s'",
         owned_device->name().c_str(), std::string(info.name().get()).c_str(),
         parent_names_[info.node_index()].c_str());
  }

  return zx::ok(std::nullopt);
}

void CompositeNodeSpecV1::SetupCompositeDevice(
    fuchsia_driver_index::wire::MatchedCompositeNodeSpecInfo info) {
  ZX_ASSERT(!has_composite_device_);
  ZX_ASSERT(info.has_composite() && info.composite().has_driver_info() &&
            info.composite().driver_info().has_url() && info.composite().has_composite_name());
  ZX_ASSERT(info.has_node_index() && info.has_num_nodes() && info.has_node_names() &&
            info.has_primary_index());
  ZX_ASSERT(info.has_name());

  auto parent_names = std::vector<std::string>(info.node_names().count());
  for (size_t i = 0; i < info.node_names().count(); i++) {
    parent_names[i] = std::string(info.node_names()[i].get());
  }
  parent_names_ = parent_names;

  auto fidl_driver_info = info.composite().driver_info();
  MatchedDriverInfo matched_driver_info = {
      .colocate = fidl_driver_info.has_colocate() && fidl_driver_info.colocate(),
      .is_dfv2 = !fidl_driver_info.has_driver_url(),
      .is_fallback = fidl_driver_info.has_is_fallback() && fidl_driver_info.is_fallback(),
      .package_type = fidl_driver_info.package_type(),
      .component_url = std::string(fidl_driver_info.url().get()),
  };

  CompositeNodeSpecInfo composite_info = {
      .spec_name = name(),
      .driver = matched_driver_info,
      .composite_name = std::string(info.name().get()),
      .primary_index = info.primary_index(),
      .parent_names = std::move(parent_names),
  };

  device_manager_.AddCompositeDeviceFromSpec(composite_info, std::move(metadata_));
  has_composite_device_ = true;
  metadata_ = fbl::Array<std::unique_ptr<Metadata>>();
}

fdd::wire::CompositeInfo CompositeNodeSpecV1::GetCompositeInfo(fidl::AnyArena& arena) const {
  if (has_composite_device_) {
    auto composite_info = device_manager_.GetCompositeInfoForSpec(arena, name());
    if (composite_info.is_ok()) {
      return composite_info.value();
    }
  }

  fidl::VectorView<fdd::wire::CompositeParentNodeInfo> parents(arena, size());
  return fdd::wire::CompositeInfo::Builder(arena)
      .name(fidl::StringView(arena, name().c_str()))
      .node_info(fdd::wire::CompositeNodeInfo::WithParents(arena, parents))
      .Build();
}

}  // namespace composite_node_specs
