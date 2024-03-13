// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ld/abi.h>
#include <lib/ld/module.h>
#include <stdint.h>

#include "test-start.h"

extern "C" int64_t a();

extern "C" int64_t TestStart() {
  // We expect to return 17 here. a() returns 13. We add 1 for every global object in the module
  // list. This includes the executable, libld-dep-a.so, and ld.so.1. On Fuchsia for out of process
  // tests we will also have a dependency on the libzircon.so.
#if defined(__Fuchsia__) && !defined(IN_PROCESS_TEST)
  constexpr int kExtraDeps = 0;
#else
  constexpr int kExtraDeps = 1;
#endif

  // Use a() to ensure we get a DT_NEEDED on libld-dep-a.so even with --as-needed.
  int64_t symbolic_deps = a();

  for (const auto* head = &ld::abi::_ld_abi.loaded_modules.get()->link_map; head;
       head = head->next.get()) {
    symbolic_deps += reinterpret_cast<const ld::abi::Abi<>::Module*>(head)->symbols_visible;
  }

  return symbolic_deps + kExtraDeps;
}
