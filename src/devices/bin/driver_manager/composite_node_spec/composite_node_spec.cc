// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/composite_node_spec/composite_node_spec.h"

CompositeNodeSpec::CompositeNodeSpec(CompositeNodeSpecCreateInfo create_info)
    : name_(create_info.name) {
  parent_specs_ = std::vector<bool>(create_info.size, false);
}

zx::result<std::optional<DeviceOrNode>> CompositeNodeSpec::BindParent(
    fuchsia_driver_index::wire::MatchedNodeGroupInfo info, const DeviceOrNode& device_or_node) {
  ZX_ASSERT(info.has_node_index());
  auto node_index = info.node_index();
  if (node_index >= parent_specs_.size()) {
    return zx::error(ZX_ERR_OUT_OF_RANGE);
  }

  if (parent_specs_[node_index]) {
    return zx::error(ZX_ERR_ALREADY_BOUND);
  }

  auto result = BindParentImpl(info, device_or_node);
  if (result.is_ok()) {
    parent_specs_[node_index] = true;
  }

  return result;
}
