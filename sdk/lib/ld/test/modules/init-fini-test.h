// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_TEST_MODULES_INIT_FINI_TEST_H_
#define LIB_LD_TEST_MODULES_INIT_FINI_TEST_H_

#include "startup-symbols.h"

// This header defines the `Check` function used by generated init-fini-*.cc
// tests. They expect a global variable to be in a certain state before it is
// updated.

// Check that the `gInitFiniState` variable is the expected value. If it is,
// the variable is incremented. If it isn't then `gInitFiniState` is set to the
// negative representation of what was expected, as a debugging breadcrumb.
inline void Check(int expected_value) {
  if (gInitFiniState == expected_value) {
    ++gInitFiniState;
  } else {
    gInitFiniState = -expected_value;
  }
}

#endif  // LIB_LD_TEST_MODULES_INIT_FINI_TEST_H_
