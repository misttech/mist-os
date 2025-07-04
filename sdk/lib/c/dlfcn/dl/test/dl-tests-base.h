// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_C_DLFCN_DL_TEST_DL_TESTS_BASE_H_
#define LIB_C_DLFCN_DL_TEST_DL_TESTS_BASE_H_

#include <lib/fit/result.h>

#include <gtest/gtest.h>

#include "../../dl_phdr_info.h"
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

  // Whether the test fixture's dlclose function will run finalizers.
  static constexpr bool kDlCloseCanRunFinalizers = true;

  // Whether the test fixture's dlclose function will unload the module.
  static constexpr bool kDlCloseUnloadsModules = true;

  // This is false if the test fixture's dl_iterate_pdhr output reflects the
  // true number of loaded modules after a module is looked up by DT_SONAME.
  static constexpr bool kInaccurateLoadCountAfterSonameMatch = false;

  // Whether the test fixture can match against the DT_SONAME of a dep module
  // pending to be loaded in a linking session.
  static constexpr bool kSonameLookupInPendingDeps = true;

  // Whether the test fixture can match against the DT_SONAME of a dep module
  // that was already loaded in a linking session.
  static constexpr bool kSonameLookupInLoadedDeps = true;

  // Whether the test fixture will resolve to the first symbol it finds, even if
  // the symbol is marked weak.
  static constexpr bool kStrictLinkOrderResolution = true;

  // Test fixtures are expected to provide definitions for the following API:
  fit::result<Error, void*> DlOpen(const char* file, int mode);

  fit::result<Error, void*> DlSym(void* module, const char* ref);

  int DlIteratePhdr(DlIteratePhdrCallback* callback, void* data);
};

}  // namespace dl::testing

#endif  // LIB_C_DLFCN_DL_TEST_DL_TESTS_BASE_H_
