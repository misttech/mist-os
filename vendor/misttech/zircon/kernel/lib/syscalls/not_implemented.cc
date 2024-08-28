// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <debug.h>
#include <lib/syscalls/forward.h>
#include <trace.h>
#include <zircon/compiler.h>
#include <zircon/syscalls/types.h>
#include <zircon/types.h>

#include <object/handle.h>
#include <object/process_dispatcher.h>

#include "priv.h"

#define LOCAL_TRACE MISTOS_SYSCALLS_GLOBAL_TRACE(0)

#define NO_RETURN(attrs) RETRUN_##attrs

#define RETURN___NO_RETURN \
  do {                     \
    __UNREACHABLE;         \
  } while (0)

#define RETURN_ \
  do {          \
    return -88; \
  } while (0)


// These don't have kernel entry points.
#define VDSO_SYSCALL(...)

// Make the function a weak symbol so real impl can override it.
#define KERNEL_SYSCALL(name, type, attrs, nargs, arglist, prototype) \
  extern "C" attrs type ni_sys_##name prototype;                                \
  type ni_sys_##name prototype {                                     \
    LTRACEF("Not implemented (%s)\n", __PRETTY_FUNCTION__);          \
    RETURN_##attrs;                                                  \
  }                                                                  \
  decltype(sys_##name) sys_##name __LEAF_FN __WEAK_ALIAS("ni_sys_" #name);

#define INTERNAL_SYSCALL(...) KERNEL_SYSCALL(__VA_ARGS__)
#define BLOCKING_SYSCALL(...) KERNEL_SYSCALL(__VA_ARGS__)

#ifdef __clang__
#define _ZX_SYSCALL_ANNO(anno) __attribute__((anno))
#else
#define _ZX_SYSCALL_ANNO(anno)
#endif

#include <lib/syscalls/kernel.inc>

#undef VDSO_SYSCALL
#undef KERNEL_SYSCALL
#undef INTERNAL_SYSCALL
#undef BLOCKING_SYSCALL
