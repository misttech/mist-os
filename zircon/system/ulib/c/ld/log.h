// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_SYSTEM_ULIB_C_LD_LOG_H_
#define ZIRCON_SYSTEM_ULIB_C_LD_LOG_H_

#include <lib/ld/log-zircon.h>

#include "../asm-linkage.h"

namespace LIBC_NAMESPACE_DECL {

// TODO(https://fxbug.dev/342469121): This will later have more methods only
// visible inside a particular hermetic_source_set().  Calls outside that can
// use the ld::Log methods, which will be separately compiled inside and
// outside that hermetic link.
class Log : public ld::Log {
 public:
};

// The gLog instance is defined outside the hermetic_source_set(), so it needs
// custom name mangling to allow the reference across the hermetic boundary.
// The instance's ld::Log methods can be used freely in the rest of libc, and
// they must be used there to set its handles at startup.
[[gnu::visibility("hidden")]] extern Log gLog LIBC_ASM_LINKAGE_DECLARE(gLog);

}  // namespace LIBC_NAMESPACE_DECL

#endif  // ZIRCON_SYSTEM_ULIB_C_LD_LOG_H_
