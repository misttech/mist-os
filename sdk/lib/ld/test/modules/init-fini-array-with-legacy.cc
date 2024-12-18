// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "startup-symbols.h"

// This module tests the order of initializers and finalizers that are defined
// using both legacy DT_INIT/DT_FINI and DT_INIT_ARRAY/DT_FINI_ARRAY tags.
//
// DT_INIT should run before DT_INIT_ARRAY and DT_FINI_ARRAY should run before
// DT_FINI.

extern "C" [[gnu::retain]] void _init() {
  if (gInitFiniState == 0) {
    gInitFiniState = 201;
  } else {
    gInitFiniState = -201;
  }
}

extern "C" [[gnu::retain]] void _fini() {
  if (gInitFiniState == 203) {
    gInitFiniState = 204;
  } else {
    gInitFiniState = -204;
  }
}

namespace {

[[gnu::constructor]] void ctor_array() {
  if (gInitFiniState == 201) {
    gInitFiniState = 202;
  } else {
    gInitFiniState = -202;
  }
}

[[gnu::destructor]] void dtor_array() {
  if (gInitFiniState == 202) {
    gInitFiniState = 203;
  } else {
    gInitFiniState = -203;
  }
}

}  // namespace
