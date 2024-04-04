// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/zx/vmar.h>

#include "module.h"

namespace dl {

void ModuleHandle::Unmap(uintptr_t vaddr, size_t len) { zx::vmar::root_self()->unmap(vaddr, len); }

}  // namespace dl
