// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/composite_node_spec/composite_node_spec.h"

namespace driver_manager {

CompositeNodeSpec::CompositeNodeSpec(CompositeNodeSpecCreateInfo create_info)
    : name_(create_info.name) {
  parent_nodes_ = std::vector<std::optional<NodeWkPtr>>(create_info.parents.size(), std::nullopt);
  parent_specs_ = std::move(create_info.parents);
}

zx::result<std::optional<NodeWkPtr>> CompositeNodeSpec::BindParent(
    fuchsia_driver_framework::wire::CompositeParent composite_parent, const NodeWkPtr& node_ptr) {
  ZX_ASSERT(composite_parent.has_index());
  auto node_index = composite_parent.index();
  if (node_index >= parent_nodes_.size()) {
    return zx::error(ZX_ERR_OUT_OF_RANGE);
  }

  const std::optional<NodeWkPtr>& current_at_index = parent_nodes_[node_index];
  if (current_at_index.has_value()) {
    const NodeWkPtr& existing_node = current_at_index.value();
    // If the driver_manager::Node no longer exists (for example because of a reload) then we don't
    // want to return an ALREADY_BOUND error.
    if (!existing_node.expired()) {
      return zx::error(ZX_ERR_ALREADY_BOUND);
    }
  }

  auto result = BindParentImpl(composite_parent, node_ptr);
  if (result.is_ok()) {
    parent_nodes_[node_index] = node_ptr;
  }

  return result;
}

void CompositeNodeSpec::Remove(RemoveCompositeNodeCallback callback) {
  std::fill(parent_nodes_.begin(), parent_nodes_.end(), std::nullopt);
  RemoveImpl(std::move(callback));
}

}  // namespace driver_manager
