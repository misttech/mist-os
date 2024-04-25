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

  // TODO(caslyn): have the POSIX vs Zircon base class supply a dlopen wrapper
  // (e.g. CallDlopen). The POSIX base class can modify the module path and the
  // Zircon base class will internally call CallWithLdsvcInstalled.
  CallWithLdsvcInstalled([&]() {
    // dlopen on POSIX does not use a Loader, so set the file arg to the test
    // location from which it will retrieve the file.
    std::filesystem::path path;
#ifndef __Fuchsia__
    if (file) {
      path = elfldltl::testing::GetTestDataPath(file);
      file = path.c_str();
    }
#endif
    result = dlopen(file, mode);
  });

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
