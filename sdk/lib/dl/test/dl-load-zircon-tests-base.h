// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DL_TEST_DL_LOAD_ZIRCON_TESTS_BASE_H_
#define LIB_DL_TEST_DL_LOAD_ZIRCON_TESTS_BASE_H_

#include <lib/ld/testing/mock-loader-service.h>

#include "dl-tests-base.h"

namespace dl::testing {

// DlLoadZirconTestsBase contains testing hooks to verify that modules and/or
// dependencies were loaded by the fuchsia.ldsvc.Loader as expected. This class
// uses the MockLoaderForTest to set a mock loader as the system loader that
// `dlopen` will invoke to load VMOs.
// Tests call `Needed` to register the ordered set of dependencies the mock
// loader is expected to load.
// TODO(caslyn): comment on how the root module is loaded.
class DlLoadZirconTestsBase : public DlTestsBase {
 public:
  constexpr void ExpectRootModule(std::string_view name) {}

  constexpr void ExpectMissing(std::string_view name) {}

  constexpr void Needed(std::initializer_list<std::string_view> names) {}

  constexpr void Needed(std::initializer_list<std::pair<std::string_view, bool>> name_found_pairs) {
  }

 private:
  ld::testing::MockLoaderServiceForTest mock_;
};

}  // namespace dl::testing

#endif  // LIB_DL_TEST_DL_LOAD_ZIRCON_TESTS_BASE_H_
