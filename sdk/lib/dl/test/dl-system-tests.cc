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
    auto prefix = "";
    // TODO(https://fxbug.dev/323419430): dlopen shouldn't know if it's loading a
    // loadable_module or shared library. Shared libraries reside in a
    // lib/$libprefix directory on instrumented builds; this is a temporary hack
    // to amend the filepath of a shared library (but not a loadable module) for
    // an instrumented build so dlopen can locate the file.
    // Eventually, the mock loader will be primed with the module/shlib files
    // and this function will only pass `filename` to `TryGetTestLibVmo()` to
    // retrieve the file from the mock loader.
    if (std::string{file}.find("module") == std::string::npos) {
      prefix = LD_TEST_LIBPREFIX;
    }
    path = std::filesystem::path("test") / "lib" / prefix / file;
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

fit::result<Error, void*> DlSystemTests::DlSym(void* module, const char* ref) {
  void* result = dlsym(module, ref);
  if (!result) {
    return TakeError();
  }
  return fit::ok(result);
}

}  // namespace dl::testing
