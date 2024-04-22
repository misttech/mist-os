// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/zx/resource.h>

#include "zx_priv.h"

#define LOCAL_TRACE ZX_GLOBAL_TRACE(0)

namespace zx {

zx_status_t resource::create(const resource& parent, uint32_t options, uint64_t base, size_t len,
                             const char* name, size_t namelen, resource* result) {
  resource h;
  result->reset(h.release());
  return ZX_OK;
}

}  // namespace zx
