// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "runtime-dynamic-linker.h"

#include <assert.h>

namespace dl {

fit::result<RuntimeDynamicLinker::Error, void*> RuntimeDynamicLinker::Open(std::string name,
                                                                           int mode) {
  if (mode & ~(kOpenSymbolScopeMask | kOpenBindingModeMask | kOpenFlagsMask)) {
    return fit::error{ "invalid mode parameter" };
  }

  // TODO(http://fxbug.dev/324650368): support file retrieval.
  return fit::error { "cannot open dependency: " + name };
}
}  // namespace dl
