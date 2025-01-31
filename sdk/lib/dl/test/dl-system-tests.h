// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DL_TEST_DL_SYSTEM_TESTS_H_
#define LIB_DL_TEST_DL_SYSTEM_TESTS_H_

#include "dl-load-tests-base.h"

#ifdef __Fuchsia__
#include "dl-load-zircon-tests-base.h"
#endif

namespace dl::testing {

#ifdef __Fuchsia__
using DlSystemLoadTestsBase = DlLoadZirconTestsBase;
#else
using DlSystemLoadTestsBase = DlLoadTestsBase;
#endif

class DlSystemTests : public DlSystemLoadTestsBase {
 public:
  // This test fixture does not need to match on exact error text, since the
  // error message can vary between different system implementations.
  static constexpr bool kCanMatchExactError = false;

#ifdef __Fuchsia__
  // Musl always prioritizes a loaded module for symbol lookup.
  static constexpr bool kStrictLoadOrderPriority = true;
  // Musl does not validate flag values for dlopen's mode argument.
  static constexpr bool kCanValidateMode = false;
  // Musl will emit a "symbol not found" error for scenarios where glibc or
  // libdl will emit an "undefined symbol" error.
  static constexpr bool kEmitsSymbolNotFound = true;
  // Fuchsia's dlclose is a no-op.
  static constexpr bool kDlCloseCanRunFinalizers = false;
  static constexpr bool kDlCloseUnloadsModules = false;
#endif

  fit::result<Error, void*> DlOpen(const char* file, int mode);

  fit::result<Error> DlClose(void* module);

  static fit::result<Error, void*> DlSym(void* module, const char* ref);

  static int DlIteratePhdr(DlIteratePhdrCallback, void* data);

  // ExpectRootModule or Needed are called by tests when a file is expected to
  // be loaded from the file system for the first time. The following functions
  // will call DlOpen(file, RTLD_NOLOAD) to ensure that `file` is not already
  // loaded (e.g. by a previous test).
  void ExpectRootModule(std::string_view name);

  void Needed(std::initializer_list<std::string_view> names);

  void Needed(std::initializer_list<std::pair<std::string_view, bool>> name_found_pairs);

  void CleanUpOpenedFile(void* ptr) override { ASSERT_TRUE(DlClose(ptr).is_ok()); }

 private:
  // This will call the system dlopen in an OS-specific context. This method is
  // defined directly on this test fixture rather than its OS-tailored base
  // classes because the logic it performs is only needed for testing the
  // system dlopen by this test fixture.
  void* CallDlOpen(const char* file, int mode);

  // DlOpen `name` with `RTLD_NOLOAD` to ensure this will be the first time the
  // file is loaded from the filesystem.
  void NoLoadCheck(std::string_view name);
};

}  // namespace dl::testing

#endif  // LIB_DL_TEST_DL_SYSTEM_TESTS_H_
