// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <debug.h>
#include <lib/user_copy/user_ptr.h>
#include <trace.h>
#include <zircon/syscalls/types.h>
#include <zircon/types.h>

#include "priv.h"

#include <linux/errno.h>

#define LOCAL_TRACE SYSCALLS_GLOBAL_TRACE(0)

// Make the function a weak symbol so real impl can override it.
#define KERNEL_SYSCALL(name, type, attrs, nargs, arglist, prototype) \
  attrs type sys_##name prototype __attribute__((weak));             \
  attrs type sys_##name prototype {                                  \
    LTRACEF("not implemented (%s)\n", __PRETTY_FUNCTION__);          \
    return -ENOSYS;                                                  \
  }

#define INTERNAL_SYSCALL(...) KERNEL_SYSCALL(__VA_ARGS__)
#define BLOCKING_SYSCALL(...) KERNEL_SYSCALL(__VA_ARGS__)

#include <lib/syscalls/kernel.inc>
