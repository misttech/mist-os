// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/task/kernel.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/vfs/file_system.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/util/weak_wrapper.h>
#include <zircon/errors.h>

#include <fbl/alloc_checker.h>
#include <ktl/string_view.h>

#include <ktl/enforce.h>

namespace starnix {

Kernel::Kernel(const ktl::string_view& _cmdline)
    : kthreads_(KernelThreads::New(util::WeakPtr(this))), cmdline{ktl::move(_cmdline)} {}

Kernel::~Kernel() = default;

fit::result<zx_status_t, fbl::RefPtr<Kernel>> Kernel::New(const ktl::string_view& cmdline) {
  fbl::AllocChecker ac;
  fbl::RefPtr<Kernel> kernel = fbl::AdoptRef(new (&ac) Kernel(cmdline));
  if (!ac.check()) {
    return fit::error(ZX_ERR_NO_MEMORY);
  }

  return fit::ok(ktl::move(kernel));
}

uint64_t Kernel::get_next_mount_id() { return next_mount_id.next(); }

uint64_t Kernel::get_next_namespace_id() { return next_namespace_id.next(); }

}  // namespace starnix
