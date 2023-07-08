// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/composite_node_spec/composite_node_spec_manager.h"

#include <utility>

#include "src/devices/lib/log/log.h"

namespace fdd = fuchsia_driver_development;
namespace fdi = fuchsia_driver_index;
namespace fdf = fuchsia_driver_framework;

CompositeNodeSpecManager::CompositeNodeSpecManager(CompositeManagerBridge *bridge)
    : bridge_(bridge) {}

fit::result<fdf::CompositeNodeSpecError> CompositeNodeSpecManager::AddSpec(
    fuchsia_driver_framework::wire::CompositeNodeSpec fidl_spec,
    std::unique_ptr<CompositeNodeSpec> spec) {
  ZX_ASSERT(spec);
  ZX_ASSERT(fidl_spec.has_name() && fidl_spec.has_parents() && !fidl_spec.parents().empty());

  auto name = std::string(fidl_spec.name().get());
  if (specs_.find(name) != specs_.end()) {
    LOGF(ERROR, "Duplicate composite node spec %.*s", static_cast<int>(name.size()), name.data());
    return fit::error(fdf::CompositeNodeSpecError::kAlreadyExists);
  }

  auto parent_count = fidl_spec.parents().count();
  AddToIndexCallback callback =
      [this, spec_impl = std::move(spec), name,
       parent_count](zx::result<fuchsia_driver_index::DriverIndexAddCompositeNodeSpecResponse>
                         result) mutable {
        if (!result.is_ok()) {
          if (result.status_value() == ZX_ERR_NOT_FOUND) {
            specs_[name] = std::move(spec_impl);
            return;
          }

          LOGF(ERROR, "CompositeNodeSpecManager::AddCompositeNodeSpec failed: %d",
               result.status_value());
          return;
        }

        if (result->node_names().size() != parent_count) {
          LOGF(
              WARNING,
              "DriverIndexAddCompositeNodeSpecResponse node_names count doesn't match node_count.");
          return;
        }

        specs_[name] = std::move(spec_impl);

        // Now that there is a new composite node spec, we can tell the bridge to attempt binds
        // again.
        bridge_->BindNodesForCompositeNodeSpec();
      };

  bridge_->AddSpecToDriverIndex(fidl_spec, std::move(callback));
  return fit::ok();
}

zx::result<std::vector<CompositeNodeAndDriver>> CompositeNodeSpecManager::BindParentSpec(
    fdi::wire::MatchedCompositeNodeParentInfo match_info, const DeviceOrNode &device_or_node,
    bool enable_multibind) {
  if (!match_info.has_specs() || match_info.specs().empty()) {
    LOGF(ERROR, "MatchedCompositeNodeParentInfo needs to contain as least one composite node spec");
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  // Go through each spec until we find an available one with an unbound parent. If
  // |enable_multibind| is true, then we will go through every spec.
  auto found_parent_spec = false;
  std::vector<CompositeNodeAndDriver> node_and_drivers;
  for (auto spec_info : match_info.specs()) {
    if (!spec_info.has_composite()) {
      continue;
    }

    if (!spec_info.has_name() || !spec_info.has_node_index() || !spec_info.has_num_nodes() ||
        !spec_info.has_node_names()) {
      LOGF(WARNING, "MatchedCompositeNodeSpecInfo missing field(s)");
      continue;
    }

    auto &name = spec_info.name();
    auto &node_index = spec_info.node_index();
    auto &num_nodes = spec_info.num_nodes();
    auto &driver = spec_info.composite();
    auto &node_names = spec_info.node_names();

    if (node_index >= num_nodes) {
      LOGF(WARNING, "MatchedCompositeNodeSpecInfo node_index is out of bounds.");
      continue;
    }

    if (node_names.count() != num_nodes) {
      LOGF(WARNING, "MatchedCompositeNodeSpecInfo num_nodes doesn't match node_names count.");
      continue;
    }

    auto name_val = std::string(name.get());
    if (specs_.find(name_val) == specs_.end()) {
      LOGF(ERROR, "Missing composite node spec %s", name_val.c_str());
      continue;
    }

    if (!specs_[name_val]) {
      LOGF(ERROR, "Stored composite node spec in %s is null", name_val.c_str());
      continue;
    }

    auto &spec = specs_[name_val];
    auto result = spec->BindParent(spec_info, device_or_node);

    if (result.is_error()) {
      if (result.error_value() != ZX_ERR_ALREADY_BOUND) {
        LOGF(ERROR, "Failed to bind node: %s", result.status_string());
      }
      continue;
    }

    found_parent_spec = true;
    auto composite_node = result.value();
    if (composite_node.has_value() && driver.has_driver_info()) {
      node_and_drivers.push_back(
          CompositeNodeAndDriver{.driver = driver.driver_info(), .node = composite_node.value()});
    }

    if (!enable_multibind) {
      return zx::ok(std::move(node_and_drivers));
    }
  }

  if (found_parent_spec) {
    return zx::ok(std::move(node_and_drivers));
  }

  return zx::error(ZX_ERR_NOT_FOUND);
}

zx::result<std::vector<CompositeNodeAndDriver>> CompositeNodeSpecManager::BindParentSpec(
    fdi::MatchedCompositeNodeParentInfo match_info, const DeviceOrNode &device_or_node,
    bool enable_multibind) {
  fidl::Arena<> arena;
  return BindParentSpec(fidl::ToWire(arena, std::move(match_info)), device_or_node,
                        enable_multibind);
}

std::vector<fdd::wire::CompositeInfo> CompositeNodeSpecManager::GetCompositeInfo(
    fidl::AnyArena &arena) const {
  std::vector<fdd::wire::CompositeInfo> composites;
  for (auto &[name, spec] : specs_) {
    if (spec) {
      composites.push_back(spec->GetCompositeInfo(arena));
    }
  }
  return composites;
}
