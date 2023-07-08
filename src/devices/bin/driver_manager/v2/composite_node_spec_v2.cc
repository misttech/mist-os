// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/v2/composite_node_spec_v2.h"

namespace fdd = fuchsia_driver_development;

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
    auto node_names = std::vector<std::string>(info.node_names().count());
    for (size_t i = 0; i < info.node_names().count(); i++) {
      node_names[i] = std::string(info.node_names()[i].get());
    }
    parent_set_collector_ = ParentSetCollector(std::string(info.name().get()),
                                               std::move(node_names), info.primary_index());
  }

  zx::result<> add_result = parent_set_collector_->AddNode(info.node_index(), *node_ptr);
  if (add_result.is_error()) {
    return add_result.take_error();
  }

  auto composite_node = parent_set_collector_->TryToAssemble(node_manager_, dispatcher_);
  if (composite_node.is_error()) {
    if (composite_node.status_value() != ZX_ERR_SHOULD_WAIT) {
      return composite_node.take_error();
    }
    return zx::ok(std::nullopt);
  }

  return zx::ok(composite_node.value());
}

fdd::wire::CompositeInfo CompositeNodeSpecV2::GetCompositeInfo(fidl::AnyArena& arena) const {
  auto composite_info =
      fdd::wire::CompositeInfo::Builder(arena).name(fidl::StringView(arena, name().c_str()));
  if (!parent_set_collector_) {
    fidl::VectorView<fdd::wire::CompositeParentNodeInfo> parents(arena, size());
    composite_info.node_info(fdd::wire::CompositeNodeInfo::WithParents(arena, parents));
    return composite_info.Build();
  }

  composite_info.driver(driver_url_)
      .primary_index(parent_set_collector_->primary_index())
      .node_info(fdd::wire::CompositeNodeInfo::WithParents(
          arena, parent_set_collector_->GetParentInfo(arena)));

  std::optional<std::weak_ptr<dfv2::Node>> composite_node =
      parent_set_collector_->completed_composite_node();
  if (composite_node) {
    if (auto node_ptr = composite_node->lock(); node_ptr) {
      composite_info.topological_path(node_ptr->MakeTopologicalPath());
    }
  }
  return composite_info.Build();
}

}  // namespace dfv2
