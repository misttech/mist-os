// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "runtime-dynamic-linker.h"

namespace dl {

Module* RuntimeDynamicLinker::FindModule(Soname name) {
  if (auto it = std::find(loaded_modules_.begin(), loaded_modules_.end(), name);
      it != loaded_modules_.end()) {
    // TODO(https://fxbug.dev/328135195): increase reference count.
    // TODO(https://fxbug.dev/326120230): update flags
    Module& found = *it;
    return &found;
  }
  return nullptr;
}

fit::result<Error, Module*> RuntimeDynamicLinker::CheckOpen(const char* file, int mode) {
  if (mode & ~(kOpenSymbolScopeMask | kOpenBindingModeMask | kOpenFlagsMask)) {
    return fit::error{Error{"invalid mode parameter"}};
  }
  if (!file || !strlen(file)) {
    return fit::error{Error{"TODO(https://fxbug.dev/324136831): nullptr for file is unsupported."}};
  }
  return fit::ok(FindModule(Soname{file}));
}

}  // namespace dl
