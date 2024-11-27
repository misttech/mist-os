// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/zx/event.h"

namespace zx {

zx_status_t event::create(uint32_t options, event* out) {
  KernelHandle<EventDispatcher> kernel_handle;
  zx_rights_t rights;

  zx_status_t result = EventDispatcher::Create(options, &kernel_handle, &rights);
  if (result != ZX_OK) {
    return result;
  }

  HandleOwner handle = Handle::Make(ktl::move(kernel_handle), rights);
  if (!handle) {
    return ZX_ERR_NO_MEMORY;
  }

  fbl::AllocChecker ac;
  auto value = fbl::MakeRefCountedChecked<zx::Value>(&ac, ktl::move(handle));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  out->reset(value);

  return ZX_OK;
}

}  // namespace zx
