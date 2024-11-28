// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/composite_node_spec_impl.h"

#include "src/devices/bin/driver_manager/node_property_conversion.h"

namespace fdd = fuchsia_driver_development;

namespace driver_manager {

CompositeNodeSpecImpl::CompositeNodeSpecImpl(CompositeNodeSpecCreateInfo create_info,
                                             async_dispatcher_t* dispatcher,
                                             NodeManager* node_manager)
    : CompositeNodeSpec(std::move(create_info)),
      parent_set_collector_(std::nullopt),
      dispatcher_(dispatcher),
      node_manager_(node_manager) {}

zx::result<std::optional<NodeWkPtr>> CompositeNodeSpecImpl::BindParentImpl(
    fuchsia_driver_framework::wire::CompositeParent composite_parent, const NodeWkPtr& node_ptr) {
  ZX_ASSERT(composite_parent.has_index());

  if (!composite_info_.has_value()) {
    ZX_ASSERT(composite_parent.has_composite());
    auto composite = fidl::ToNatural(composite_parent.composite());
    composite_info_ = composite;
  }

  auto& spec = composite_info_->spec();
  ZX_ASSERT(spec.has_value());
  auto& matched_driver = composite_info_->matched_driver();
  ZX_ASSERT(matched_driver.has_value());
  auto& spec_name = spec.value().name();
  ZX_ASSERT(spec_name.has_value());
  auto spec_name_value = spec_name.value();
  auto& composite_driver = matched_driver.value().composite_driver();
  ZX_ASSERT(composite_driver.has_value());
  auto& driver_info = composite_driver.value().driver_info();
  ZX_ASSERT(driver_info.has_value());
  auto& parent_names = matched_driver.value().parent_names();
  ZX_ASSERT(parent_names.has_value());
  auto& primary_index = matched_driver.value().primary_parent_index();
  auto& url = driver_info.value().url();
  ZX_ASSERT(url.has_value());

  if (!parent_set_collector_) {
    parent_set_collector_ =
        ParentSetCollector(spec_name_value, parent_names.value(), primary_index.value_or(0));
    driver_url_ = url.value();
  }

  ZX_ASSERT(parent_specs().size() > composite_parent.index());

  std::vector<fuchsia_driver_framework::NodeProperty2> properties;
  properties.reserve(parent_specs()[composite_parent.index()].properties().size());
  for (auto& property : parent_specs()[composite_parent.index()].properties()) {
    properties.emplace_back(ToProperty2(property));
  }

  zx::result<> add_result =
      parent_set_collector_->AddNode(composite_parent.index(), properties, node_ptr);
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

void CompositeNodeSpecImpl::RemoveImpl(RemoveCompositeNodeCallback callback) {
  if (!parent_set_collector_) {
    callback(zx::ok());
    return;
  }

  // TODO(https://fxbug.dev/42075799): Once we start enforcing the multibind composite flag, move
  // the parent nodes back to the orphaned nodes if they can't multibind.
  auto node = parent_set_collector_->completed_composite_node();
  if (node && !node->expired()) {
    node->lock()->RemoveCompositeNodeForRebind(std::move(callback));
    parent_set_collector_.reset();
    driver_url_ = "";
    composite_info_.reset();
    return;
  }

  parent_set_collector_.reset();
  driver_url_ = "";
  composite_info_.reset();
  callback(zx::ok());
}

fdd::wire::CompositeNodeInfo CompositeNodeSpecImpl::GetCompositeInfo(fidl::AnyArena& arena) const {
  auto composite_info = fdd::wire::CompositeNodeInfo::Builder(arena);
  if (!parent_set_collector_) {
    fidl::VectorView<fidl::StringView> parent_topological_paths(arena, size());
    composite_info.parent_topological_paths(parent_topological_paths);
    return composite_info.Build();
  }

  if (composite_info_.has_value()) {
    composite_info.composite(fdd::wire::CompositeInfo::WithComposite(
        arena, fidl::ToWire(arena, composite_info_.value())));
  }

  composite_info.parent_topological_paths(parent_set_collector_->GetParentTopologicalPaths(arena));

  std::optional<NodeWkPtr> composite_node = parent_set_collector_->completed_composite_node();
  if (composite_node) {
    if (auto node_ptr = composite_node->lock(); node_ptr) {
      composite_info.topological_path(node_ptr->MakeTopologicalPath());
    }
  }
  return composite_info.Build();
}

}  // namespace driver_manager
