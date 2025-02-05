// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DL_TEST_DL_TESTS_BASE_H_
#define LIB_DL_TEST_DL_TESTS_BASE_H_

// Avoid symbol conflict between <ld/abi/abi.h> and <link.h>
#pragma push_macro("_r_debug")
#undef _r_debug
#define _r_debug not_using_system_r_debug
#include <link.h>
#pragma pop_macro("_r_debug")

#include <lib/fit/result.h>

#include <gtest/gtest.h>

#include "../error.h"

namespace dl::testing {

using DlIteratePhdrCallback = int(dl_phdr_info*, size_t, void*);

// The main purpose of this base class is to document and declare the testing
// API that each test fixture is expected to provide definitions for. Default
// values for shared feature flags are also defined here so that test fixtures
// may support testing features independently from each other.
class DlTestsBase : public ::testing::Test {
 public:
  // These variables are indicators to GTEST of whether the test fixture
  // supports the associated feature so that it may skip related tests if not
  // supported:
  // Whether the test fixture can support matching error text exactly. This
  // allows different system implementations to pass tests that check whether an
  // expected error occurred without needing to adhere to the exact error
  // verbiage.
  static constexpr bool kCanMatchExactError = true;

  // A "Symbol not found" error is emitted for any undefined symbol error.
  static constexpr bool kEmitsSymbolNotFound = false;

  // Whether the dlopen implementation validates the mode argument.
  static constexpr bool kCanValidateMode = true;

  // Whether the test fixture will always prioritize a loaded module in symbol
  // resolution, regardless of whether it is a global module.
  static constexpr bool kStrictLoadOrderPriority = false;

  // Whether the test fixture supports dynamic TLS.
  static constexpr bool kSupportsDynamicTls = true;

  // Whether the test fixture's dlclose function will run finalizers.
  static constexpr bool kDlCloseCanRunFinalizers = true;

  // Whether the test fixture's dlclose function will unload the module.
  static constexpr bool kDlCloseUnloadsModules = true;

  // Whether the test fixture's implementation provides dl_iterate_pdhr.
  static constexpr bool kProvidesDlIteratePhdr = true;

  // Test fixtures are expected to provide definitions for the following API:
  fit::result<Error, void*> DlOpen(const char* file, int mode);

  fit::result<Error, void*> DlSym(void* module, const char* ref);

  int DlIteratePhdr(DlIteratePhdrCallback, void* data);
};

}  // namespace dl::testing

#endif  // LIB_DL_TEST_DL_TESTS_BASE_H_
