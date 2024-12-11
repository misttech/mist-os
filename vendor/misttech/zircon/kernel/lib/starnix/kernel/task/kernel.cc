// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/task/kernel.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/vfs/file_system.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <trace.h>
#include <zircon/errors.h>

#include <fbl/alloc_checker.h>
#include <ktl/string_view.h>

#include "../kernel_priv.h"

#include <ktl/enforce.h>

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

namespace starnix {

Kernel::Kernel(BString cmdline, KernelFeatures features)
    : kthreads_(KernelThreads::New()),
      features_(features),
      cmdline_{ktl::move(cmdline)},
      device_registry_(DeviceRegistry::Default()),
      weak_factory_(this) {
  LTRACE_ENTRY_OBJ;
  kthreads_.set_kernel(weak_factory_.GetWeakPtr());
}

Kernel::~Kernel() { LTRACE_EXIT_OBJ; }

fit::result<zx_status_t, fbl::RefPtr<Kernel>> Kernel::New(BString cmdline,
                                                          KernelFeatures features) {
  fbl::AllocChecker ac;
  fbl::RefPtr<Kernel> kernel = fbl::AdoptRef(new (&ac) Kernel(ktl::move(cmdline), features));
  if (!ac.check()) {
    return fit::error(ZX_ERR_NO_MEMORY);
  }
  return fit::ok(ktl::move(kernel));
}

}  // namespace starnix
