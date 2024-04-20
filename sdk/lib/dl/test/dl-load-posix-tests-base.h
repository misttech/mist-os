// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DL_TEST_DL_LOAD_POSIX_TESTS_BASE_H_
#define LIB_DL_TEST_DL_LOAD_POSIX_TESTS_BASE_H_

#include <string_view>

#include "dl-tests-base.h"

namespace dl::testing {

// TODO(https://fxbug.dev/323419430): For now DlLoadPosixTestsBase contains
// empty hooks so that dl-load-tests.cc can call them and compile. Eventually,
// we will refactor the DlLoadPosixTestsBase and DlLoadFuchsiaTestsBase in
// such a way where there will be a common base class where these hooks will
// open the files by name to verify they exist in the test data. The POSIX
// test fixture will only use this base class, whereas the Zircon test fixture
// will use a subclass of that which statically overrides those methods.
class DlLoadPosixTestsBase : public DlTestsBase {
 public:
  constexpr void ExpectRootModule(std::string_view name) {}

  constexpr void ExpectMissing(std::string_view name) {}

  constexpr void Needed(std::initializer_list<std::string_view> names) {}

  // TODO(https://fxbug.dev/323419430): This can use TryGetTestLib to assert
  // that the file does not exist.
  constexpr void Needed(std::initializer_list<std::pair<std::string_view, bool>> name_found_pairs) {
  }
};

}  // namespace dl::testing

#endif  // LIB_DL_TEST_DL_LOAD_POSIX_TESTS_BASE_H_
