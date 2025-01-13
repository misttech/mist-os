// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_TEST_MODULES_INIT_FINI_TEST_H_
#define LIB_LD_TEST_MODULES_INIT_FINI_TEST_H_

#include "startup-symbols.h"

// This header defines the `Register*` functions called by init/fini functions
// in generated init-fini-array-*.cc tests. These functions will "register" the
// given identity value when an init/fini function is run.

inline void RegisterInit(int id) {
  if (gRegisterInitFini) {
    gRegisterInitFini->RegisterInit(id);
  }
}

inline void RegisterFini(int id) {
  if (gRegisterInitFini) {
    gRegisterInitFini->RegisterFini(id);
  }
}

#endif  // LIB_LD_TEST_MODULES_INIT_FINI_TEST_H_
