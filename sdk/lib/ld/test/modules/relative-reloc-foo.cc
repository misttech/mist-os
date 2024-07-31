// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <stdint.h>

namespace {

const int val = 17;
const int* val_ptr = &val;

}  // namespace

extern "C" [[gnu::visibility("default")]] int64_t foo() {
  const int** foo_ptr;
  // We need to take the address of `val_ptr` so the compiler doesn't optimize
  // this as a pc-relative load to `val`. Similarly, we need the compiler to
  // lose all provenance information about this pointer so it doesn't just
  // optimize this to a pc-relative load or  more likely just load 17.
  __asm__("" : "=r"(foo_ptr) : "0"(&val_ptr));
  return **foo_ptr;
}
