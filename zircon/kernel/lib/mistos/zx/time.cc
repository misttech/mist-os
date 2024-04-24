// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/zx/time.h"

#include <trace.h>

#include <kernel/deadline.h>
#include <kernel/thread.h>

#include "zx_priv.h"

#define LOCAL_TRACE ZX_GLOBAL_TRACE(0)

namespace zx {

zx_status_t nanosleep(zx::time deadline) {
  LTRACEF("nseconds %" PRIi64 "\n", deadline.get());

  if (deadline.get() <= 0) {
    return ZX_OK;
  }

  const zx_time_t now = current_time();
  const Deadline slackDeadline(deadline.get(), TimerSlack::none());

  // This syscall is declared as "blocking", so a higher layer will automatically
  // retry if we return ZX_ERR_INTERNAL_INTR_RETRY.
  return Thread::Current::SleepEtc(slackDeadline, Interruptible::Yes, now);
}

}  // namespace zx
