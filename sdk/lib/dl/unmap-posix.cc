// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <sys/mman.h>

#include "module.h"

namespace dl {

void Module::Unmap(uintptr_t vaddr, size_t len) { munmap(reinterpret_cast<void*>(vaddr), len); }

}  // namespace dl
