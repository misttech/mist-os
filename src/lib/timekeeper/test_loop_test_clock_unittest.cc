// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/lib/timekeeper/test_loop_test_clock.h"

#include <lib/async-testing/test_loop.h>
#include <lib/async/cpp/time.h>

#include <gtest/gtest.h>

namespace timekeeper {
namespace {

TEST(TestLoopTestClockTest, Increment) {
  async::TestLoop loop;
  TestLoopTestClock clock(&loop);

  auto time1 = clock.MonotonicNow();

  EXPECT_EQ(async::Now(loop.dispatcher()), time1 + zx::duration(1));

  auto time2 = clock.MonotonicNow();
  EXPECT_GT(time2, time1);
}

}  // namespace
}  // namespace timekeeper
