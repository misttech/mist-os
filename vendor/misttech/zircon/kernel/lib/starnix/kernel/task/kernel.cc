// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/task/kernel.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <zircon/errors.h>

#include <fbl/alloc_checker.h>

namespace starnix {

Kernel::~Kernel() = default;

fit::result<zx_status_t, fbl::RefPtr<Kernel>> Kernel::New(fbl::String cmdline) {
  fbl::AllocChecker ac;

  fbl::RefPtr<Kernel> kernel = fbl::AdoptRef(new (&ac) Kernel(cmdline));
  if (!ac.check()) {
    return fit::error(ZX_ERR_NO_MEMORY);
  }

  return fit::ok(ktl::move(kernel));
}

}  // namespace starnix
