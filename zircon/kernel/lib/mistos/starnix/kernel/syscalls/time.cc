// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/syscalls/time.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix_uapi/time.h>
#include <lib/mistos/util/time.h>
#include <trace.h>

#include <cerrno>
#include <cstdint>

#include "../kernel_priv.h"

#include <linux/time.h>

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

using namespace starnix_uapi;

namespace starnix {

fit::result<Errno> sys_clock_gettime(const CurrentTask& current_task, const clockid_t which_clock,
                                     UserRef<struct timespec> tp_addr) {
  LTRACEF_LEVEL(2, "which_clock 0x%x\n", which_clock);
  int64_t nanos = 0;
  if (which_clock < 0) {
  } else {
    switch (which_clock) {
      case CLOCK_REALTIME:
      case CLOCK_REALTIME_COARSE:
        // TODO (Herrrera) we must use UTC clock here.
        nanos = zx_clock_get_monotonic();
        break;
      case CLOCK_MONOTONIC:
      case CLOCK_MONOTONIC_COARSE:
      case CLOCK_MONOTONIC_RAW:
      case CLOCK_BOOTTIME:
        nanos = zx_clock_get_monotonic();
        break;
      case CLOCK_THREAD_CPUTIME_ID:
        break;
      case CLOCK_PROCESS_CPUTIME_ID:
        break;
      default:
        return fit::error(errno(EINVAL));
    }
  }
  if (nanos == 0) {
    return fit::error(errno(ENOSYS));
  } else {
    struct timespec tv{nanos / NANOS_PER_SECOND, nanos % NANOS_PER_SECOND};
    auto result = current_task.write_object(tp_addr, tv);
    if (result.is_error())
      return result.take_error();
    return fit::ok();
  }
}

}  // namespace starnix
