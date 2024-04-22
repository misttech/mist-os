// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/zx/event.h>
#include <trace.h>

#include "zx_priv.h"

#define LOCAL_TRACE ZX_GLOBAL_TRACE(0)

namespace zx {

zx_status_t event::create(uint32_t options, event* result) {
  LTRACEF("options 0x%x\n", options);

  if (options != 0u)
    return ZX_ERR_INVALID_ARGS;

  KernelHandle<EventDispatcher> kernel_handle;
  zx_rights_t rights;

  zx_status_t status = EventDispatcher::Create(options, &kernel_handle, &rights);
  if (status != ZX_OK) {
    return status;
  }

  result->reset(kernel_handle.release());
  return ZX_OK;
}

}  // namespace zx
