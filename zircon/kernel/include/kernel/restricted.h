// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_INCLUDE_KERNEL_RESTRICTED_H_
#define ZIRCON_KERNEL_INCLUDE_KERNEL_RESTRICTED_H_

#include <lib/user_copy/user_ptr.h>
#include <zircon/syscalls-next.h>

// Routines to support restricted mode.

// Enter restricted mode on the current thread.
zx_status_t RestrictedEnter(uint32_t options, uintptr_t vector_table_ptr, uintptr_t context);

#endif  // ZIRCON_KERNEL_INCLUDE_KERNEL_RESTRICTED_H_
