// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "runtime-dynamic-linker.h"

#include <string>

namespace dl {

Module* RuntimeDynamicLinker::FindModule(const Soname& name) {
  if (auto it = std::find(loaded_modules_.begin(), loaded_modules_.end(), name);
      it != loaded_modules_.end()) {
    Module& found = *it;
    return &found;
  }
  return nullptr;
}

fit::result<Error, void*> RuntimeDynamicLinker::Open(const char* file, int mode) {
  if (mode & ~(kOpenSymbolScopeMask | kOpenBindingModeMask | kOpenFlagsMask)) {
    return fit::error{"invalid mode parameter"};
  }

  if (!file) {
    return fit::error{
        "TODO(https://fxbug.dev/324136831): return modules list that includes startup modules"};
  }

  Soname name{file};

  // Return a reference to the module if it is already loaded.
  if (auto* found = FindModule(name)) {
    // TODO(https://fxbug.dev/328135195): increase reference count.
    // TODO(https://fxbug.dev/326120230): update flags
    return fit::ok(found);
  }
  // TODO(https://fxbug.dev/323418587): a module will be created and added to
  // loaded_modules_ only after it has been successfully loaded/relocated/etc.
  // For now, create a new module and add it to loaded_modules_ so that we can
  // look up and re-use the same module in basic tests.
  fbl::AllocChecker ac;
  loaded_modules_.push_back(Module::Create(name, ac));

  // TODO(https://fxbug.dev/326138362): support & test RTLD_NOLOAD.

  // TODO(https://fxbug.dev/324650368): support file retrieval.
  return fit::error<Error>{"cannot open dependency: ", name.c_str()};
}

}  // namespace dl
