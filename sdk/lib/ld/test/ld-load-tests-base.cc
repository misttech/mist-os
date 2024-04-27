// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ld-load-tests-base.h"

#include <lib/elfldltl/testing/get-test-data.h>
#include <lib/elfldltl/testing/test-pipe-reader.h>

#include <gtest/gtest.h>

namespace ld::testing {

void LdLoadTestsBase::InitLog(fbl::unique_fd& log_fd) {
  ASSERT_FALSE(log_);
  log_ = std::make_unique<elfldltl::testing::TestPipeReader>();
  ASSERT_NO_FATAL_FAILURE(log_->Init(log_fd));
}

void LdLoadTestsBase::ExpectLog(std::string_view expected_log) {
  ASSERT_TRUE(log_);
  std::string log = std::move(*std::exchange(log_, {})).Finish();
  EXPECT_EQ(log, expected_log);
}

std::string LdLoadTestsBase::CollectLog() {
  if (!log_) {
    ADD_FAILURE() << "CollectLog() called without successful InitLog()";
    return {};
  }
  return std::move(*std::exchange(log_, {})).Finish();
}

LdLoadTestsBase::~LdLoadTestsBase() {
  // The log should have been collected by ExpectLog.  If the test is bailing
  // out early anyway, then don't confuse things with more failures.
  if (!::testing::Test::HasFatalFailure()) {
    EXPECT_FALSE(log_);
  }
}

void LdLoadTestsBase::Needed(std::initializer_list<std::string_view> names) {
  // The POSIX dynamic linker will just do `open` system calls to find files.
  // It runs chdir'd to the directory where they're found.  Nothing else done
  // here in the test harness affects the lookups it does or verifies that it
  // does the expected set in the expected order.  So this just verifies that
  // each SONAME in the list is an existing test file.
  for (std::string_view name : names) {
    ASSERT_TRUE(elfldltl::testing::GetTestLib(name)) << name;
  }
}

void LdLoadTestsBase::Needed(
    std::initializer_list<std::pair<std::string_view, bool>> name_found_pairs) {
  for (auto [name, found] : name_found_pairs) {
    if (found) {
      ASSERT_TRUE(elfldltl::testing::GetTestLib(name)) << name;
    } else {
      ASSERT_FALSE(elfldltl::testing::TryGetTestLib(name)) << name;
    }
  }
}

}  // namespace ld::testing
