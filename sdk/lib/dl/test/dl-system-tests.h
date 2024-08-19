// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DL_TEST_DL_SYSTEM_TESTS_H_
#define LIB_DL_TEST_DL_SYSTEM_TESTS_H_

#ifdef __Fuchsia__
#include "dl-load-zircon-tests-base.h"
#endif

#include "dl-load-tests-base.h"

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
  // Fuchisa's musl will always prioritize a loaded module for symbol lookup.
  static constexpr bool kStrictLoadOrderPriority = true;
  // Fuchsia's musl implementation of dlopen does not validate flag values for
  // the mode argument.
  static constexpr bool kCanValidateMode = false;
  // Fuchsia's musl will emit a "symbol not found" error for scenarios where
  // glibc or libdl will emit an "undefined symbol" error.
  static constexpr bool kEmitsSymbolNotFound = true;
#endif

  fit::result<Error, void*> DlOpen(const char* file, int mode);

  static fit::result<Error> DlClose(void* module);

  static fit::result<Error, void*> DlSym(void* module, const char* ref);

  // TODO(https://fxbug.dev/354043838): We can convert these functions to be
  // wrappers around ExpectLoadModule/Needed when we create stamped executables
  // in //sdk/lib/ld/test/modules/BUILD.gn.
  // ExpectRootModule or Needed are called by tests when a file is expected to
  // be loaded from the file system for the first time. The following functions
  // will call DlOpen(file, RTLD_NOLOAD) to ensure that `file` is not already
  // loaded (e.g. by a previous test).
  void ExpectRootModuleNotLoaded(std::string_view name);

  void ExpectNeededNotLoaded(std::initializer_list<std::string_view> names);

 private:
  // This will call the system dlopen in an OS-specific context. This method is
  // defined directly on this test fixture rather than its OS-tailored base
  // classes because the logic it performs is only needed for testing the
  // system dlopen by this test fixture.
  void* CallDlOpen(const char* file, int mode);
};

}  // namespace dl::testing

#endif  // LIB_DL_TEST_DL_SYSTEM_TESTS_H_
