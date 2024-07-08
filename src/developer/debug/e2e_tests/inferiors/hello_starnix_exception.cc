// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <stdio.h>
#include <stdlib.h>

int main() {
  // Generate an architectural exception (page fault) which will cause a restricted
  // exception exit to Starnix. Starnix's exception handling logic will then issue a
  // backtrace request exception.
  *reinterpret_cast<volatile char*>(0x0) = 1;
  return 0;
}
