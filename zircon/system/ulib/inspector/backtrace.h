// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_SYSTEM_ULIB_INSPECTOR_BACKTRACE_H_
#define ZIRCON_SYSTEM_ULIB_INSPECTOR_BACKTRACE_H_

#include <zircon/types.h>

#include <fbl/string.h>

#include "inspector/inspector.h"

namespace inspector {

// Writes to |f|: the stack from ngunwind and the markup context for modules found in the stack. If
// the stack and SCS mismatch, the SCS and markup context for all modules will also be written.
void print_backtrace_markup(FILE* f, zx_handle_t process, zx_handle_t thread,
                            bool skip_markup_context);

}  // namespace inspector

#endif  // ZIRCON_SYSTEM_ULIB_INSPECTOR_BACKTRACE_H_
