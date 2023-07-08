// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/v2/parent_set_collector.h"

namespace fdd = fuchsia_driver_development;

namespace dfv2 {

zx::result<> ParentSetCollector::AddNode(uint32_t index, std::weak_ptr<Node> node) {
  ZX_ASSERT(index < parents_.size());
  if (!parents_[index].expired()) {
    return zx::error(ZX_ERR_ALREADY_BOUND);
  }
  parents_[index] = std::move(node);
  return zx::ok();
}

void ParentSetCollector::RemoveNode(uint32_t index) {
  ZX_ASSERT(index < parents_.size());
  parents_[index] = std::weak_ptr<Node>();
}

zx::result<std::shared_ptr<Node>> ParentSetCollector::TryToAssemble(
    NodeManager* node_manager, async_dispatcher_t* dispatcher) {
  if (completed_composite_node_ && !completed_composite_node_->expired()) {
    return zx::error(ZX_ERR_ALREADY_EXISTS);
  }

  std::vector<Node*> parents;
  for (auto& node : parents_) {
    auto parent = node.lock();
    if (!parent) {
      return zx::error(ZX_ERR_SHOULD_WAIT);
    }
    parents.push_back(parent.get());
  }

  auto result = Node::CreateCompositeNode(composite_name_, std::move(parents), parent_names_, {},
                                          node_manager, dispatcher, primary_index_);
  if (result.is_error()) {
    return result.take_error();
  }

  LOGF(INFO, "Built composite node '%s' for completed composite node spec",
       composite_name_.c_str());
  completed_composite_node_.emplace(result.value());
  return zx::ok(result.value());
}

fidl::VectorView<fdd::wire::CompositeParentNodeInfo> ParentSetCollector::GetParentInfo(
    fidl::AnyArena& arena) const {
  fidl::VectorView<fdd::wire::CompositeParentNodeInfo> parents(arena, parents_.size());
  for (uint32_t i = 0; i < parents_.size(); i++) {
    auto parent_info = fdd::wire::CompositeParentNodeInfo::Builder(arena).name(
        fidl::StringView(arena, std::string(parent_names_[i])));
    if (auto node = parents_[i].lock(); node) {
      parent_info.device(arena, node->MakeTopologicalPath());
    }

    parents[i] = parent_info.Build();
  }
  return parents;
}

}  // namespace dfv2
