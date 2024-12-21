// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/syscalls/forward.h>
#include <trace.h>
#include <zircon/syscalls.h>

#include "../../priv.h"

#define LOCAL_TRACE MISTOS_SYSCALLS_GLOBAL_TRACE(0)

long sys_a0024_sched_yield() {
  auto status = sys_thread_legacy_yield(0);
  if (status == ZX_ERR_INVALID_ARGS) {
    return -EINVAL;
  }
  return 0;
}
