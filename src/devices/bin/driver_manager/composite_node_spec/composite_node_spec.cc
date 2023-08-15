// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/composite_node_spec/composite_node_spec.h"

CompositeNodeSpec::CompositeNodeSpec(CompositeNodeSpecCreateInfo create_info)
    : name_(create_info.name) {
  parent_specs_ = std::vector<std::optional<DeviceOrNode>>(create_info.size, std::nullopt);
}

zx::result<std::optional<DeviceOrNode>> CompositeNodeSpec::BindParent(
    fuchsia_driver_index::wire::MatchedCompositeNodeSpecInfo info,
    const DeviceOrNode& device_or_node) {
  ZX_ASSERT(info.has_node_index());
  auto node_index = info.node_index();
  if (node_index >= parent_specs_.size()) {
    return zx::error(ZX_ERR_OUT_OF_RANGE);
  }

  const std::optional<DeviceOrNode>& current_at_index = parent_specs_[node_index];
  if (current_at_index.has_value()) {
    const DeviceOrNode& existing = current_at_index.value();
    const std::weak_ptr<dfv2::Node>* existing_node =
        std::get_if<std::weak_ptr<dfv2::Node>>(&existing);
    // If the dfv2::Node no longer exists (for example because of a reload) then we don't
    // want to return an ALREADY_BOUND error.
    if (!existing_node || !existing_node->expired()) {
      return zx::error(ZX_ERR_ALREADY_BOUND);
    }
  }

  auto result = BindParentImpl(info, device_or_node);
  if (result.is_ok()) {
    parent_specs_[node_index] = device_or_node;
  }

  return result;
}

void CompositeNodeSpec::Remove(RemoveCompositeNodeCallback callback) {
  std::fill(parent_specs_.begin(), parent_specs_.end(), std::nullopt);
  RemoveImpl(std::move(callback));
}
