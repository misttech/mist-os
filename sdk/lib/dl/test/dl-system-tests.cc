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
  // Call dlopen in an OS-specific context.
  void* result = CallDlOpen(file, mode);
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

#ifdef __Fuchsia__
// Call dlopen with the mock fuchsia_ldsvc::Loader installed.
void* DlSystemTests::CallDlOpen(const char* file, int mode) {
  void* result;
  // TODO(caslyn): verify and clear mock expectations.
  CallWithLdsvcInstalled([&]() { result = dlopen(file, mode); });
  return result;
}
#else
// Call dlopen with the test path modified for POSIX.
void* DlSystemTests::CallDlOpen(const char* file, int mode) {
  std::filesystem::path path;
  if (file) {
    path = elfldltl::testing::GetTestDataPath(file);
    file = path.c_str();
  }
  return dlopen(file, mode);
}
#endif

}  // namespace dl::testing
