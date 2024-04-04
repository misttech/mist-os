// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "module.h"

namespace dl {

// TODO(https://fxbug.dev/332769914): Define the destructor in the posix/zircon
// implementations.
// On destruction, unmap the module's load image.
ModuleHandle::~ModuleHandle() {
  if (size_t size = abi_module_.vaddr_end - abi_module_.vaddr_start; size > 0) {
    Unmap(abi_module_.vaddr_start, size);
  }
}

}  // namespace dl
