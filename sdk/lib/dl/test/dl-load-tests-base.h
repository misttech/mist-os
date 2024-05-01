// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DL_TEST_DL_LOAD_TESTS_BASE_H_
#define LIB_DL_TEST_DL_LOAD_TESTS_BASE_H_

#include <lib/elfldltl/fd.h>
#include <lib/elfldltl/mmap-loader.h>
#include <lib/fit/function.h>

#include <string_view>

#include <fbl/unique_fd.h>

#include "../diagnostics.h"
#include "dl-tests-base.h"

namespace dl::testing {

// This is a common base class for test fixture classes and is the active
// base class for non-Fuchsia tests. All Fuchsia-specific test fixtures should
// override this (e.g. via DlLoadZirconTestsBase).
class DlLoadTestsBase : public DlTestsBase {
 public:
  using File = elfldltl::UniqueFdFile<Diagnostics>;
  using Loader = elfldltl::MmapLoader;

  // The Expect/Needed API checks that the test files exist in test paths as
  // expected, or are missing if the test files are expected to not be found.
  static void ExpectRootModule(std::string_view name);

  static void ExpectMissing(std::string_view name);

  static void Needed(std::initializer_list<std::string_view> names);

  static void Needed(std::initializer_list<std::pair<std::string_view, bool>> name_found_pairs);

  // There is no particular loader to install on non-Fuchsia systems during
  // tests, so this function simply runs the function it was passed.
  static void CallWithLdsvcInstalled(fit::closure func) { func(); }

  // Check that startup modules are not retrieved from the filesystem.
  static void FileCheck(std::string_view filename);

  // Retrieve a file from the filesystem.
  std::optional<File> RetrieveFile(Diagnostics& diag, std::string_view filename);
};

}  // namespace dl::testing

#endif  // LIB_DL_TEST_DL_LOAD_TESTS_BASE_H_
