// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/syscalls/forward.h>
#include <lib/user_copy/user_ptr.h>
#include <trace.h>

#include "../priv.h"

// clang format off
#include <lib/mistos/linux_uapi/typedefs.h>
// clang format on

#define LOCAL_TRACE SYSCALLS_GLOBAL_TRACE(0)

long sys_statx(int dfd, user_in_ptr<const char> path, unsigned flags, unsigned mask,
               user_out_ptr<struct statx> buffer) {
  LTRACEF_LEVEL(2, "dfd=%d path=%p flags=%x mask=%x buf=%p \n", dfd, path.get(), flags, mask,
                buffer.get());

  return -EINVAL;
}
