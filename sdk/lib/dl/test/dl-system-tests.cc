// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "dl-system-tests.h"

#include <dlfcn.h>
#include <lib/elfldltl/testing/get-test-data.h>

#include <filesystem>

namespace dl::testing {

namespace {

// Consume the pending dlerror() state after a <dlfcn.h> call that failed.
// The return value is suitable for any fit:result<Error, ...> return value.
fit::error<Error> TakeError() {
  const char* error_str = dlerror();
  EXPECT_TRUE(error_str);
  return fit::error<Error>{"%s", error_str};
}

}  // namespace

fit::result<Error, void*> DlSystemTests::DlOpen(const char* file, int mode) {
  void* result;
  if (!file || !strlen(file)) {
    result = dlopen(file, mode);
  } else {
    std::filesystem::path path;
#ifdef __Fuchsia__
    // Use the lib prefix for library paths to the same prefix used in libld,
    // which generates the testing modules used by libdl.
    path = std::filesystem::path("test") / "lib" / LD_TEST_LIBPREFIX / file;
#else
    path = elfldltl::testing::GetTestDataPath(file);
#endif
    result = dlopen(path.c_str(), mode);
  }
  if (!result) {
    return TakeError();
  }
  return fit::ok(result);
}

}  // namespace dl::testing
