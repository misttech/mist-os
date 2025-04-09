// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/composite_node_spec/composite_node_spec_manager.h"

#include <utility>

#include "src/devices/lib/log/log.h"

namespace driver_manager {
namespace fdd = fuchsia_driver_development;
namespace fdfw = fuchsia_driver_framework;

CompositeNodeSpecManager::CompositeNodeSpecManager(CompositeManagerBridge* bridge)
    : bridge_(bridge) {}

void CompositeNodeSpecManager::AddSpec(
    fuchsia_driver_framework::wire::CompositeNodeSpec fidl_spec,
    std::unique_ptr<CompositeNodeSpec> spec,
    fit::callback<void(fit::result<fdfw::CompositeNodeSpecError>)> callback) {
  ZX_ASSERT(spec);
  ZX_ASSERT(fidl_spec.has_name() && ((fidl_spec.has_parents() && !fidl_spec.parents().empty()) ||
                                     fidl_spec.has_parents2() && !fidl_spec.parents2().empty()));

  auto name = std::string(fidl_spec.name().get());
  if (specs_.find(name) != specs_.end()) {
    LOGF(ERROR, "Duplicate composite node spec %.*s", static_cast<int>(name.size()), name.data());
    return callback(fit::error(fdfw::CompositeNodeSpecError::kAlreadyExists));
  }

  AddToIndexCallback index_callback = [this, spec_impl = std::move(spec),
                                       callback = std::move(callback),
                                       name](zx::result<> result) mutable {
    if (!result.is_ok()) {
      LOGF(ERROR, "CompositeNodeSpecManager::AddCompositeNodeSpec failed: %d",
           result.status_value());
      callback(fit::error(fdfw::CompositeNodeSpecError::kDriverIndexFailure));
      return;
    }

    specs_[name] = std::move(spec_impl);

    // Now that there is a new composite node spec, we can tell the bridge to attempt binds
    // again.
    bridge_->BindNodesForCompositeNodeSpec();
    callback(fit::ok());
  };

  bridge_->AddSpecToDriverIndex(fidl_spec, std::move(index_callback));
}

zx::result<BindSpecResult> CompositeNodeSpecManager::BindParentSpec(
    fidl::AnyArena& arena, fidl::VectorView<fdfw::wire::CompositeParent> composite_parents,
    const NodeWkPtr& node_ptr, bool enable_multibind) {
  if (composite_parents.empty()) {
    LOGF(ERROR, "composite_parents needs to contain as least one composite parent.");
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  // Go through each spec until we find an available one with an unbound parent. If
  // |enable_multibind| is true, then we will go through every spec.
  std::vector<fdfw::wire::CompositeParent> bound_composite_parents;
  std::vector<CompositeNodeAndDriver> node_and_drivers;
  for (auto composite_parent : composite_parents) {
    if (!composite_parent.has_composite()) {
      LOGF(WARNING, "CompositeParent missing composite.");
      continue;
    }

    if (!composite_parent.has_index()) {
      LOGF(WARNING, "CompositeParent missing index.");
      continue;
    }

    auto& composite = composite_parent.composite();
    auto& index = composite_parent.index();

    if (!composite.has_matched_driver()) {
      continue;
    }

    auto& matched_driver = composite.matched_driver();

    if (!matched_driver.has_composite_driver() || !matched_driver.has_parent_names()) {
      LOGF(WARNING, "CompositeDriverMatch does not have all needed fields.");
      continue;
    }

    if (!composite.has_spec()) {
      LOGF(WARNING, "CompositeInfo missing spec.");
      continue;
    }

    auto& spec_info = composite.spec();

    if (!spec_info.has_name() || (!spec_info.has_parents() && !spec_info.has_parents2())) {
      LOGF(WARNING, "CompositeNodeSpec missing name or parents.");
      continue;
    }

    auto check_parents = [&](auto parents) {
      if (index >= parents.count()) {
        LOGF(WARNING, "CompositeParent index is out of bounds.");
        return true;
      }
      if (matched_driver.parent_names().count() != parents.count()) {
        LOGF(WARNING, "Parent names count does not match the spec parent count.");
        return true;
      }
      return false;
    };

    if (spec_info.has_parents()) {
      if (check_parents(spec_info.parents())) {
        continue;
      }
    } else {
      if (check_parents(spec_info.parents2())) {
        continue;
      }
    }

    auto& name = spec_info.name();
    auto name_val = std::string(name.get());
    if (specs_.find(name_val) == specs_.end()) {
      LOGF(ERROR, "Missing composite node spec %s", name_val.c_str());
      continue;
    }

    if (!specs_[name_val]) {
      LOGF(ERROR, "Stored composite node spec in %s is null", name_val.c_str());
      continue;
    }

    auto& spec = specs_[name_val];
    auto result = spec->BindParent(composite_parent, node_ptr);

    if (result.is_error()) {
      if (result.error_value() != ZX_ERR_ALREADY_BOUND) {
        LOGF(ERROR, "Failed to bind node: %s", result.status_string());
      }
      continue;
    }

    bound_composite_parents.push_back(composite_parent);
    auto composite_node = result.value();
    if (composite_node.has_value()) {
      node_and_drivers.push_back(CompositeNodeAndDriver{.driver = matched_driver.composite_driver(),
                                                        .node = composite_node.value()});
    }

    if (!enable_multibind) {
      break;
    }
  }

  // Copy the content into a new wire vector view since we had it stored in a std::vector.
  auto wire_parents =
      fidl::VectorView<fdfw::wire::CompositeParent>(arena, bound_composite_parents.size());
  int i = 0;
  for (auto& bound_composite_parent : bound_composite_parents) {
    wire_parents[i++] = bound_composite_parent;
  }

  if (!wire_parents.empty()) {
    return zx::ok(BindSpecResult{wire_parents, std::move(node_and_drivers)});
  }

  return zx::error(ZX_ERR_NOT_FOUND);
}

void CompositeNodeSpecManager::Rebind(std::string spec_name,
                                      std::optional<std::string> restart_driver_url_suffix,
                                      fit::callback<void(zx::result<>)> rebind_spec_completer) {
  if (specs_.find(spec_name) == specs_.end()) {
    LOGF(WARNING, "Spec %s is not available for rebind", spec_name.c_str());
    rebind_spec_completer(zx::error(ZX_ERR_NOT_FOUND));
    return;
  }

  auto rebind_request_callback =
      [this, spec_name,
       rebind_spec_completer = std::move(rebind_spec_completer)](zx::result<> result) mutable {
        if (!result.is_ok()) {
          rebind_spec_completer(result.take_error());
          return;
        }
        OnRequestRebindComplete(spec_name, std::move(rebind_spec_completer));
      };
  bridge_->RequestRebindFromDriverIndex(spec_name, restart_driver_url_suffix,
                                        std::move(rebind_request_callback));
}

std::vector<fdd::wire::CompositeNodeInfo> CompositeNodeSpecManager::GetCompositeInfo(
    fidl::AnyArena& arena) const {
  std::vector<fdd::wire::CompositeNodeInfo> composites;
  for (auto& [name, spec] : specs_) {
    if (spec) {
      composites.push_back(spec->GetCompositeInfo(arena));
    }
  }
  return composites;
}

void CompositeNodeSpecManager::OnRequestRebindComplete(
    std::string spec_name, fit::callback<void(zx::result<>)> rebind_spec_completer) {
  specs_[spec_name]->Remove(
      [this, completer = std::move(rebind_spec_completer)](zx::result<> result) mutable {
        if (!result.is_ok()) {
          completer(result.take_error());
          return;
        }

        bridge_->BindNodesForCompositeNodeSpec();
        completer(zx::ok());
      });
}
}  // namespace driver_manager
