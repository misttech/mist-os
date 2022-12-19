// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "test-data.h"

__EXPORT const int foo = 17;

__EXPORT int BasicSymbol() { return 1; }

__EXPORT int NeedsPlt() { return BasicSymbol() + 1; }

__EXPORT int NeedsGot() {
  auto* f = NeedsPlt;
  // Don't let the compiler pessimize this to a PLT call or just inline NeedsPlt.
  __asm__("" : "=r"(f) : "0"(f));
  return f() + 1;
}
