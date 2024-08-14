// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/intel-display/util/poll-until.h"

#include <lib/driver/testing/cpp/scoped_global_logger.h>

#include <gtest/gtest.h>

namespace intel_display {

namespace {

// A predicate whose value changes and tracks how many times it's been invoked.
class PredicateCounter {
 public:
  explicit PredicateCounter(int threshold) : threshold_(threshold) {}

  bool increment_and_compare() {
    ++counter_;
    return counter_ >= threshold_;
  }

  int counter() const { return counter_; }

 private:
  int counter_ = 0;
  int threshold_;
};

class PollUntilTest : public ::testing::Test {
 private:
  fdf_testing::ScopedGlobalLogger logger_;
};

TEST_F(PollUntilTest, TrueOnFirstPoll) {
  PredicateCounter always_true(0);

  const bool poll_result =
      PollUntil([&] { return always_true.increment_and_compare(); }, zx::nsec(1), 10);
  EXPECT_EQ(true, poll_result);
  EXPECT_EQ(1, always_true.counter());
}

TEST_F(PollUntilTest, TrueAfterTwoPolls) {
  PredicateCounter always_true(2);

  const bool poll_result =
      PollUntil([&] { return always_true.increment_and_compare(); }, zx::nsec(1), 10);
  EXPECT_EQ(true, poll_result);
  EXPECT_EQ(2, always_true.counter());
}

TEST_F(PollUntilTest, TrueAfterMaximuPolls) {
  PredicateCounter always_true(10);

  const bool poll_result =
      PollUntil([&] { return always_true.increment_and_compare(); }, zx::nsec(1), 10);
  EXPECT_EQ(true, poll_result);
  EXPECT_EQ(10, always_true.counter());
}

TEST_F(PollUntilTest, Timeout) {
  PredicateCounter always_true(100);

  const bool poll_result =
      PollUntil([&] { return always_true.increment_and_compare(); }, zx::nsec(1), 10);
  EXPECT_EQ(false, poll_result);
  EXPECT_EQ(11, always_true.counter());
}

}  // namespace

}  // namespace intel_display
