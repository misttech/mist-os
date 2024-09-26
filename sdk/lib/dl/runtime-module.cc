// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "runtime-module.h"

namespace dl {

ModuleRefList RuntimeModule::TraverseDeps(Diagnostics& diag, const RuntimeModule& root_module) {
  ModuleRefList ordered_queue;

  auto enqueue = [&diag, &ordered_queue](const RuntimeModule& module) -> bool {
    // Skip if the module has already been enqueued, preventing circular
    // dependencies.
    if (std::ranges::find(ordered_queue, module.name(), &RuntimeModule::name) !=
        ordered_queue.end()) {
      return true;
    }
    return ordered_queue.push_back(diag, "module dependencies list", &module);
  };

  if (!enqueue(root_module)) {
    return {};
  }

  // Build the BFS-ordered queue: an indexed-for-loop iterates over the list
  // since it can be invalidated as modules are enqueued.
  for (size_t i = 0; i < ordered_queue.size(); ++i) {
    const RuntimeModule& module = *ordered_queue[i];
    // Enqueue the first-level dependencies of the current module.
    if (!std::ranges::all_of(module.direct_deps(), enqueue,
                             [](const RuntimeModule* m) -> const RuntimeModule& { return *m; })) {
      return {};
    }
  }

  return ordered_queue;
}

}  // namespace dl
