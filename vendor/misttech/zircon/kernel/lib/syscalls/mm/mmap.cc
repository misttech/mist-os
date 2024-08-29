// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/syscalls/forward.h>
#include <trace.h>

#include "../priv.h"

#include <linux/errno.h>

#define LOCAL_TRACE MISTOS_SYSCALLS_GLOBAL_TRACE(2)

long sys_a0012_brk(unsigned long brk) {
  LTRACEF_LEVEL(2, "brk=0x%lx\n", brk);
  return -ENOSYS;
}
