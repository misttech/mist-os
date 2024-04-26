// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <sys/mman.h>

#include "module.h"

namespace dl {

ModuleHandle::~ModuleHandle() {
  if (vaddr_size() > 0) {
    munmap(reinterpret_cast<void*>(static_cast<uintptr_t>(abi_module_.vaddr_start)), vaddr_size());
  }
}

}  // namespace dl
