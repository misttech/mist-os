// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "dl-system-tests.h"

#include <dlfcn.h>
#include <lib/elfldltl/testing/get-test-data.h>

#include <filesystem>

namespace dl::testing {

namespace {

Error ErrorResult() {
  const char* error_str = dlerror();
  EXPECT_TRUE(error_str);
  return Error{error_str};
}

}  // namespace

fit::result<Error, void*> DlSystemTests::DlOpen(const char* name, int mode) {
  std::filesystem::path path = elfldltl::testing::GetTestDataPath(".") / name;
  void* result = dlopen(path.c_str(), mode);
  if (!result) {
    return fit::error{ErrorResult()};
  }
  return fit::ok(result);
}

}  // namespace dl::testing
