// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DL_TEST_DL_IMPL_TESTS_H_
#define LIB_DL_TEST_DL_IMPL_TESTS_H_

#include "../runtime-dynamic-linker.h"
#include "dl-tests-base.h"

namespace dl::testing {

class DlImplTests : public DlTestsBase {
 public:
  // Error messages in tests can be matched exactly with this test fixture,
  // since the error message returned from the libdl implementation will be the
  // same regardless of the OS.
  static constexpr bool kCanMatchExactError = true;

  fit::result<Error, void*> DlOpen(const char* name, int mode);

 private:
  RuntimeDynamicLinker dynamic_linker_;
};

}  // namespace dl::testing

#endif  // LIB_DL_TEST_DL_IMPL_TESTS_H_
