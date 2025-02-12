// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "startup-random.h"

#include <lib/ld/tls.h>
#include <zircon/syscalls.h>
#include <zircon/tls.h>

#include <cstdint>

#include "../setjmp/fuchsia/jmp_buf.h"

namespace LIBC_NAMESPACE_DECL {

void InitStartupRandom() {
  // Initialize stack-protector canary value first thing.  It never lives
  // permanently anywhere except in every thread descriptor, where it's found
  // via the <zircon/tls.h> Fuchsia Compiler ABI layout.  The main thread
  // gets a random value here, and thread creation copies it from the
  // creating thread into the new thread.  Do the setjmp manglers in the same
  // call to avoid the overhead of two system calls.  That means we need a
  // temporary buffer on the stack, which we then want to clear out so the
  // values don't leak there.
  struct randoms {
    uintptr_t stack_guard;
    decltype(gJmpBufManglers) setjmp_manglers;
  } randoms;
  _zx_cprng_draw(&randoms, sizeof(randoms));
  *ld::TpRelative<uintptr_t>(ZX_TLS_STACK_GUARD_OFFSET) = randoms.stack_guard;
  gJmpBufManglers = randoms.setjmp_manglers;
  // Zero the stack temporaries.
  randoms = (struct randoms){};
  // Tell the compiler that the value is used, so it doesn't optimize
  // out the zeroing as dead stores.
  __asm__("# keepalive %0" ::"m"(randoms));
}

}  // namespace LIBC_NAMESPACE_DECL
