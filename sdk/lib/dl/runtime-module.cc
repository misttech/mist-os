// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "runtime-module.h"

namespace dl {

bool RuntimeModule::ReifyModuleTree(Diagnostics& diag) {
  if (!module_tree_.is_empty()) {
    return true;
  }

  auto enqueue = [this, &diag](const RuntimeModule& module) -> bool {
    return module_tree_.push_back(diag, "module dependencies list", &module);
  };

  // This module is the root and always the first entry in the module tree.
  if (!enqueue(*this)) {
    return false;
  }

  auto enqueue_unique = [&](const RuntimeModule& module) -> bool {
    // Skip if the module has already been enqueued, preventing circular
    // dependencies.
    if (std::ranges::find(module_tree_, module.name(), &RuntimeModule::name) !=
        module_tree_.end()) {
      return true;
    }
    return enqueue(module);
  };

  // Build the BFS-ordered queue: this uses an indexed-for-loop to iterate
  // over the list since it can't be invalidated as modules are enqueued.
  for (size_t i = 0; i < module_tree_.size(); ++i) {
    const RuntimeModule& module = *module_tree_[i];
    // Enqueue the first-level dependencies of the current module.
    if (!std::ranges::all_of(module.GetDirectDeps(), enqueue_unique)) {
      return false;
    }
  }

  return true;
}

}  // namespace dl
