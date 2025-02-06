// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "runtime-module.h"

#include <lib/ld/decoded-module.h>

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

// TODO(https://fxbug.dev/382527519): support synchronization with initializers.
void RuntimeModule::InitializeModuleTree() {
  // If this module is already initialized, then all its dependencies are
  // initialized, so return early.
  if (initialized_) {
    return;
  }

  // Initializers are run in reverse-order of the dependency tree, so that init
  // funcs for dependencies are run before the init funcs of dependent modules.
  // The init functions for this module will be run last.
  for (const RuntimeModule& module : std::ranges::reverse_view(module_tree())) {
    // TODO(https://fxbug.dev/374810148): Introduce non-const version of ModuleTree.
    const_cast<RuntimeModule&>(module).Initialize();
  }
}

void RuntimeModule::Initialize() {
  // If this module's init functions have already run, don't run them again.
  if (initialized_) {
    return;
  }
  initialized_ = true;
  module().init.CallInit(load_bias());
}

dl_phdr_info RuntimeModule::MakePhdrInfo(uint64_t global_loaded, uint64_t global_unloaded) const {
  // In practice module().phdrs.size() will never exceed this limit, and would
  // cause a UBSan fatal error if it did. The corner case for this is if e_phnum
  // is set to 0xffff (i.e. PT_XNUM). We statically assert here to ensure we
  // don't run into this case.
  static_assert(ld::kMaxPhdrs < std::numeric_limits<uint16_t>::max());
  return {
      .dlpi_addr = module().link_map.addr,
      .dlpi_name = module().link_map.name.get(),
      .dlpi_phdr = reinterpret_cast<const ElfW(Phdr)*>(module().phdrs.data()),
      .dlpi_phnum = static_cast<uint16_t>(module().phdrs.size()),
      .dlpi_adds = global_loaded,
      .dlpi_subs = global_unloaded,
      .dlpi_tls_modid = tls_module_id(),
      // TODO(https://fxbug.dev/331421403): Include TLS data when supported.
      .dlpi_tls_data = nullptr,
  };
}

}  // namespace dl
