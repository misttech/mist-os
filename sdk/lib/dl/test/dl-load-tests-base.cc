// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "dl-load-tests-base.h"

#include <fcntl.h>
#include <lib/elfldltl/testing/get-test-data.h>
#include <lib/ld/abi.h>

#include <gtest/gtest.h>

namespace dl::testing {

void DlLoadTestsBase::ExpectRootModule(std::string_view name) {
  ASSERT_TRUE(elfldltl::testing::GetTestLib(name)) << name;
}

void DlLoadTestsBase::ExpectMissing(std::string_view name) {
  ASSERT_FALSE(elfldltl::testing::TryGetTestLib(name)) << name;
}

void DlLoadTestsBase::Needed(std::initializer_list<std::string_view> names) {
  // The POSIX dynamic linker will just do `open` system calls to find files.
  // It runs chdir'd to the directory where they're found.  Nothing else done
  // here in the test harness affects the lookups it does or verifies that it
  // does the expected set in the expected order.  So this just verifies that
  // each SONAME in the list is an existing test file.
  for (std::string_view name : names) {
    ASSERT_TRUE(elfldltl::testing::GetTestLib(name)) << name;
  }
}

void DlLoadTestsBase::Needed(
    std::initializer_list<std::pair<std::string_view, bool>> name_found_pairs) {
  for (auto [name, found] : name_found_pairs) {
    if (found) {
      ASSERT_TRUE(elfldltl::testing::GetTestLib(name)) << name;
    } else {
      ASSERT_FALSE(elfldltl::testing::TryGetTestLib(name)) << name;
    }
  }
}

void DlLoadTestsBase::FileCheck(std::string_view filename) {
  EXPECT_NE(filename, ld::abi::Abi<>::kSoname.str());
}

std::optional<DlLoadTestsBase::File> DlLoadTestsBase::RetrieveFile(Diagnostics& diag,
                                                                   std::string_view filename) {
  FileCheck(filename);
  std::filesystem::path path = elfldltl::testing::GetTestDataPath(filename);
  if (fbl::unique_fd fd{open(path.c_str(), O_RDONLY)}) {
    return File{std::move(fd), diag};
  }
  diag.SystemError("cannot open ", filename);
  return std::nullopt;
}

}  // namespace dl::testing
