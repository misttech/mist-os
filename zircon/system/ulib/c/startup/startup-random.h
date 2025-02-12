// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_SYSTEM_ULIB_C_STARTUP_STARTUP_RANDOM_H_
#define ZIRCON_SYSTEM_ULIB_C_STARTUP_STARTUP_RANDOM_H_

#include "../asm-linkage.h"

namespace LIBC_NAMESPACE_DECL {

// This initializes the ZX_TLS_STACK_GUARD_OFFSET slot in the current (main)
// thread, as well as the setjmp manglers.
void InitStartupRandom() LIBC_ASM_LINKAGE_DECLARE(InitStartupRandom);

}  // namespace LIBC_NAMESPACE_DECL

#endif  // ZIRCON_SYSTEM_ULIB_C_STARTUP_STARTUP_RANDOM_H_
