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
  // TODO(https://fxbug.dev/348722959): Musl shouldn't retrieve file w/
  // RTLD_NOLOAD.
  static constexpr bool kRetrievesFileWithNoLoad = true;
#endif

  fit::result<Error, void*> DlOpen(const char* file, int mode);

  static fit::result<Error> DlClose(void* module);

  static fit::result<Error, void*> DlSym(void* module, const char* ref);

 private:
  // This will call the system dlopen in an OS-specific context. This method is
  // defined directly on this test fixture rather than its OS-tailored base
  // classes because the logic it performs is only needed for testing the
  // system dlopen by this test fixture.
  void* CallDlOpen(const char* file, int mode);
};

}  // namespace dl::testing

#endif  // LIB_DL_TEST_DL_SYSTEM_TESTS_H_
