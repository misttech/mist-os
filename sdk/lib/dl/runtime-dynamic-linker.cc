// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "runtime-dynamic-linker.h"

#include <assert.h>

namespace dl {

fit::result<RuntimeDynamicLinker::Error, void*> RuntimeDynamicLinker::Open(std::string name,
                                                                           int mode) {
  if (mode & ~(kOpenSymbolScopeMask | kOpenBindingModeMask | kOpenFlagsMask)) {
    return fit::error{ "TODO(https://fxbug.dev/323417704): handle invalid flags" };
  }
  return fit::error{ "TODO(https://fxbug.dev/323422024): support module loading" };
}
}  // namespace dl
