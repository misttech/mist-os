// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ld/abi.h>
#include <lib/ld/module.h>
#include <stdint.h>

#include <iterator>

#include "test-start.h"

// Note, we use extern "C" to make debugging easier than seeing mangled names.
extern "C" {

int64_t a();
int64_t b();
int64_t f();

int64_t TestStart() {
  // a should return 13
  // b should return -8
  // f should return 3
  // We expect 9 total modules.
  // 13 + -8 + 3 + 9 = 17
  auto view = ld::AbiLoadedModules(ld::abi::_ld_abi);
  return a() + b() + f() + std::distance(view.begin(), view.end());
}

}  // extern "C"
