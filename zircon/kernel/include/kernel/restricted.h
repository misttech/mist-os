// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_INCLUDE_KERNEL_RESTRICTED_H_
#define ZIRCON_KERNEL_INCLUDE_KERNEL_RESTRICTED_H_

#include <lib/user_copy/user_ptr.h>

#include <arch/exception.h>
#include <kernel/restricted_state.h>

// Routines to support restricted mode.

// Enter restricted mode on the current thread.
zx_status_t RestrictedEnter(uint32_t options, uintptr_t vector_table_ptr, uintptr_t context);

// Called as part of a synchronous exception that is to be handled by the
// normal mode in-thread exception handler.
//
// Specifically this function will:
//   1. Copy the thread state out of the exception context and into the
//      restricted state VMO.
//   2. Update the exception `iframe_t` on the stack such that the thread
//      will return from this exception to normal mode instead of restricted
//      mode. The restricted `reason code` will be set to
//      `ZX_RESTRICTED_REASON_EXCEPTION`.
//
// The caller must ensure that this is only called on a thread that is in
// restricted mode.
void RedirectRestrictedExceptionToNormalMode(RestrictedState* rs);

#endif  // ZIRCON_KERNEL_INCLUDE_KERNEL_RESTRICTED_H_
