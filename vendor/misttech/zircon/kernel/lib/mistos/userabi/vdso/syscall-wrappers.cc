// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "private.h"

// One of these macros is invoked by syscalls.inc for each syscall.

// These don't need wrappers.
#define VDSO_SYSCALL(...)
#define KERNEL_SYSCALL(...)
#define INTERNAL_SYSCALL(...)
#define BLOCKING_SYSCALL(...)

#ifdef __clang__
#define _ZX_SYSCALL_ANNO(anno) __attribute__((anno))
#else
#define _ZX_SYSCALL_ANNO(anno)
#endif

#include <lib/syscalls/syscalls.inc>

#undef VDSO_SYSCALL
#undef KERNEL_SYSCALL
#undef INTERNAL_SYSCALL
#undef BLOCKING_SYSCALL
