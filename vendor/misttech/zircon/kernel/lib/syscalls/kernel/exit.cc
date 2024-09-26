// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/syscalls/forward.h>
#include <trace.h>

#include "../priv.h"

#define LOCAL_TRACE MISTOS_SYSCALLS_GLOBAL_TRACE(0)

void __NO_RETURN do_exit(long code) {
  LTRACEF_LEVEL(2, "code=0x%lx\n", code);
  ProcessDispatcher::ExitCurrent(code);
}

long sys_a0060_exit(int32_t retcode) {
  do_exit((retcode & 0xff) << 8);
  /* NOTREACHED */
  return 0;
}
