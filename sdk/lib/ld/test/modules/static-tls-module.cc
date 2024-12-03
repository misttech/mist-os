// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/elfldltl/layout.h>

#include "startup-symbols.h"

#if !defined(HAVE_TLSDESC) || !defined(WANT_TLSDESC)
#error "//build/config:{no-,}tlsdesc should define {HAVE,WANT}_TLSDESC"
#elif HAVE_TLSDESC == WANT_TLSDESC

extern "C" [[gnu::visibility("hidden")]] void* __tls_get_addr(elfldltl::Elf<>::TlsGetAddrGot* got) {
  // Nothing useful is returned, just return the pointer that's passed in.
  return got;
}

int* get_static_tls_var() { return &gStaticTlsVar; }

#endif
