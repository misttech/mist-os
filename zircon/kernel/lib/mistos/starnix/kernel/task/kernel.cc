// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/task/kernel.h"

#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <zircon/errors.h>

#include <fbl/alloc_checker.h>

namespace starnix {

zx_status_t Kernel::New(fbl::String cmdline, fbl::RefPtr<Kernel>* out) {
  fbl::AllocChecker ac;

  fbl::RefPtr<Kernel> kernel = fbl::AdoptRef(new (&ac) Kernel(cmdline));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  *out = std::move(kernel);
  return ZX_OK;
}

}  // namespace starnix
