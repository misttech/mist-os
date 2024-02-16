// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "dl-system-tests.h"

#include <dlfcn.h>

namespace dl::testing {

fit::result<Error, void*> DlSystemTests::DlOpen(const char* name, int mode) {
  void* result = dlopen(name, mode);
  if (!result) {
    return fit::error{ErrorResult()};
  }
  return fit::ok(result);
}

Error DlSystemTests::ErrorResult() {
  const char* error_str = dlerror();
  EXPECT_TRUE(error_str);
  return Error{error_str};
}

}  // namespace dl::testing
