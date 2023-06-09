// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/v2/composite_node_spec_v2.h"

#include "src/devices/lib/log/log.h"

namespace dfv2 {

CompositeNodeSpecV2::CompositeNodeSpecV2(CompositeNodeSpecCreateInfo create_info,
                                         async_dispatcher_t* dispatcher, NodeManager* node_manager)
    : CompositeNodeSpec(std::move(create_info)),
      parent_set_collector_(std::nullopt),
      dispatcher_(dispatcher),
      node_manager_(node_manager) {}

zx::result<std::optional<DeviceOrNode>> CompositeNodeSpecV2::BindParentImpl(
    fuchsia_driver_index::wire::MatchedCompositeNodeSpecInfo info,
    const DeviceOrNode& device_or_node) {
  auto node_ptr = std::get_if<std::weak_ptr<dfv2::Node>>(&device_or_node);
  ZX_ASSERT(node_ptr);
  ZX_ASSERT(info.has_node_index() && info.has_node_index() && info.has_node_names() &&
            info.has_primary_index());
  ZX_ASSERT(info.has_composite() && info.composite().has_composite_name());
  ZX_ASSERT(info.has_name());

  if (!parent_set_collector_) {
    parent_set_collector_ = ParentSetCollector(info.node_names().count());
  }

  if (parent_set_collector_->ContainsNode(info.node_index())) {
    return zx::error(ZX_ERR_ALREADY_BOUND);
  }

  parent_set_collector_->AddNode(info.node_index(), *node_ptr);

  auto owned_node = (*node_ptr).lock();
  if (owned_node->name() == "sysmem-fidl" || owned_node->name() == "sysmem-banjo") {
    LOGF(DEBUG, "Node '%s' matched parent '%d' of composite node spec '%s'",
         owned_node->name().c_str(), info.node_index(), std::string(info.name().get()).c_str());
  } else {
    LOGF(INFO, "Node '%s' matched parent '%d' of composite node spec '%s'",
         owned_node->name().c_str(), info.node_index(), std::string(info.name().get()).c_str());
  }

  // Check if we have all the nodes for the composite node spec.
  auto completed_parents = parent_set_collector_->GetIfComplete();
  if (!completed_parents.has_value()) {
    // Parent set is not complete yet.
    return zx::ok(std::nullopt);
  }

  auto node_names = std::vector<std::string>(info.node_names().count());
  for (size_t i = 0; i < info.node_names().count(); i++) {
    node_names[i] = std::string(info.node_names()[i].get());
  }

  auto node_name = std::string(info.name().get());

  // Create a composite node for the composite node spec with our complete parent set.
  auto composite = Node::CreateCompositeNode(node_name, std::move(*completed_parents), node_names,
                                             {}, node_manager_, dispatcher_, info.primary_index());
  if (composite.is_error()) {
    // If we are returning an error we should clear out what we have.
    parent_set_collector_->RemoveNode(info.node_index());
    return composite.take_error();
  }

  LOGF(INFO, "Built composite node '%s' for completed composite node spec '%s'",
       composite.value()->name().c_str(), std::string(info.name().get()).c_str());

  completed_composite_node_.emplace(composite.value()->weak_from_this());

  // We can return a pointer, as the composite node is owned by its parents.
  return zx::ok(composite.value()->weak_from_this());
}

}  // namespace dfv2
