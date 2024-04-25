// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DL_TEST_DL_SYSTEM_TESTS_H_
#define LIB_DL_TEST_DL_SYSTEM_TESTS_H_

#ifdef __Fuchsia__
#include "dl-load-zircon-tests-base.h"
#else
#include "dl-load-posix-tests-base.h"
#endif

namespace dl::testing {

#ifdef __Fuchsia__
using DlSystemLoadTestsBase = DlLoadZirconTestsBase;
#else
using DlSystemLoadTestsBase = DlLoadPosixTestsBase;
#endif

class DlSystemTests : public DlSystemLoadTestsBase {
 public:
  // This test fixture does not need to match on exact error text, since the
  // error message can vary between different system implementations.
  static constexpr bool kCanMatchExactError = false;

#ifdef __Fuchsia__
  // Fuchsia's musl implementation of dlopen does not validate flag values for
  // the mode argument.
  static constexpr bool kCanValidateMode = false;
#endif

  fit::result<Error, void*> DlOpen(const char* file, int mode);

  static fit::result<Error, void*> DlSym(void* module, const char* ref);
};

}  // namespace dl::testing

#endif  // LIB_DL_TEST_DL_SYSTEM_TESTS_H_
