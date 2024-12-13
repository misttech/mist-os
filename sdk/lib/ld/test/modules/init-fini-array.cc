// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "startup-symbols.h"

// This module defines initializers and finalizer functions that are accumulated
// into the DT_INIT_ARRAY/DT_FINI_ARRAY section. They expect a global variable to
// be in a certain state before it is updated.

namespace {

[[gnu::constructor(101)]] void ctor0() {
  if (gInitFiniState == 0) {
    gInitFiniState = 1;
  } else {
    gInitFiniState = -1;
  }
}

[[gnu::constructor(102)]] void ctor1() {
  if (gInitFiniState == 1) {
    gInitFiniState = 2;
  } else {
    gInitFiniState = -2;
  }
}

[[gnu::constructor(103)]] void ctor2() {
  if (gInitFiniState == 2) {
    gInitFiniState = 3;
  } else {
    gInitFiniState = -3;
  }
}

// Note, functions are run in the reverse order they appear in the DT_FINI_ARRAY.
// For readability, this lists the dtors in the order in which they are run.

[[gnu::destructor(103)]] void dtor2() {
  // The first destructor to run will expect `gInitFiniState` to be the value
  // set by the last constructor that ran.
  if (gInitFiniState == 3) {
    gInitFiniState = 4;
  } else {
    gInitFiniState = -4;
  }
}

[[gnu::destructor(102)]] void dtor1() {
  if (gInitFiniState == 4) {
    gInitFiniState = 5;
  } else {
    gInitFiniState = -5;
  }
}

[[gnu::destructor(101)]] void dtor0() {
  if (gInitFiniState == 5) {
    gInitFiniState = 6;
  } else {
    gInitFiniState = -6;
  }
}

}  // namespace
