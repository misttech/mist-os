// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "dl-system-tests.h"

#include <dlfcn.h>
#include <lib/fit/defer.h>

#include <cstring>

#include <gtest/gtest.h>

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
    if (mode & RTLD_NOLOAD) {
      // Musl emits a "Library x is not already loaded" for RTLD_NOLOAD, so
      // consume any failure from dlerror here.
      dlerror();
      return fit::ok(result);
    }
    return TakeError();
  }
  TrackModule(result, std::string{file});
  return fit::ok(result);
}

fit::result<Error> DlSystemTests::DlClose(void* module) {
  auto untrack_file = fit::defer([&]() { DlSystemLoadTestsBase::UntrackModule(module); });
  if (dlclose(module)) {
    return TakeError();
  }
  return fit::ok();
}

fit::result<Error, void*> DlSystemTests::DlSym(void* module, const char* ref) {
  void* result = dlsym(module, ref);
  if (!result) {
    return TakeError();
  }
  return fit::ok(result);
}

int DlSystemTests::DlIteratePhdr(DlIteratePhdrCallback callback, void* data) {
  return dl_iterate_phdr(callback, data);
}

#ifdef __Fuchsia__

// Call dlopen with the mock fuchsia_ldsvc::Loader installed and check that all
// its Needed/Expect* expectations were satisfied before clearing them.
void* DlSystemTests::CallDlOpen(const char* file, int mode) {
  void* result;
  CallWithLdsvcInstalled([&]() { result = dlopen(file, mode); });
  VerifyAndClearNeeded();
  return result;
}

#else  // POSIX, not __Fuchsia__

// Call dlopen with the unadorned name, which the DT_RUNPATH in the host test
// executable will find in a subdirectory relative to that test executable.
void* DlSystemTests::CallDlOpen(const char* file, int mode) {
  if (file) {
    EXPECT_EQ(strchr(file, '/'), nullptr) << file;
  }
  return dlopen(file, mode);
}

#endif  // __Fuchsia__

void DlSystemTests::NoLoadCheck(std::string_view name) {
  auto result = DlOpen(std::string{name}.c_str(), RTLD_NOW | RTLD_LOCAL | RTLD_NOLOAD);
  ASSERT_TRUE(result.is_ok());
  ASSERT_EQ(result.value(), nullptr);
}

void DlSystemTests::ExpectRootModule(std::string_view name) {
  NoLoadCheck(name);
  DlSystemLoadTestsBase::ExpectRootModule(name);
}

void DlSystemTests::Needed(std::initializer_list<std::string_view> names) {
  for (auto name : names) {
    NoLoadCheck(name);
  }
  // Now add the expectation that the deps will be loaded from the filesystem.
  DlSystemLoadTestsBase::Needed(names);
}

void DlSystemTests::Needed(
    std::initializer_list<std::pair<std::string_view, bool>> name_found_pairs) {
  for (auto [name, found] : name_found_pairs) {
    if (found) {
      NoLoadCheck(name);
    }
  }
  // Now add the expectation that the deps will be loaded from the filesystem.
  DlSystemLoadTestsBase::Needed(name_found_pairs);
}

}  // namespace dl::testing
