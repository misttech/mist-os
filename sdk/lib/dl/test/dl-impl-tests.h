// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DL_TEST_DL_IMPL_TESTS_H_
#define LIB_DL_TEST_DL_IMPL_TESTS_H_

#include <fbl/unique_fd.h>

#include "../runtime-dynamic-linker.h"
#include "dl-tests-base.h"

#ifdef __Fuchsia__
#include <lib/zx/vmo.h>
#endif

namespace dl::testing {

#ifdef __Fuchsia__
class TestFuchsia {
 public:
  // TODO(https://fxbug.dev/328490762): alias to elfldltl::VmoFile<Diagnostics>
  // when we add Diagnostics support.
  using File = zx::vmo;

  static fit::result<Error, File> RetrieveFile(std::string_view filename);
};
#endif

class TestPosix {
 public:
  // TODO(https://fxbug.dev/328490762): alias to elfldltl::UniqueFdFile<Diagnostics>
  // when we add Diagnostics support.
  using File = fbl::unique_fd;

  static fit::result<Error, File> RetrieveFile(std::string_view filename);
};

template <class TestOS>
class DlImplTests : public DlTestsBase {
 public:
  // Error messages in tests can be matched exactly with this test fixture,
  // since the error message returned from the libdl implementation will be the
  // same regardless of the OS.
  static constexpr bool kCanMatchExactError = true;

  fit::result<Error, void*> DlOpen(const char* file, int mode) {
    return dynamic_linker_.Open<TestOS>(file, mode);
  }

 private:
  RuntimeDynamicLinker dynamic_linker_;
};

}  // namespace dl::testing

#endif  // LIB_DL_TEST_DL_IMPL_TESTS_H_
