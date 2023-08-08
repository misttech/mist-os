// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ld-load-tests-base.h"

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

LdLoadTestsBase::~LdLoadTestsBase() {
  // The log should have been collected by ExpectLog.  If the test is bailing
  // out early anyway, then don't confuse things with more failures.
  if (!::testing::Test::HasFatalFailure()) {
    EXPECT_FALSE(log_);
  }
}

}  // namespace ld::testing
