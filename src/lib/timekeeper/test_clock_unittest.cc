// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/lib/timekeeper/test_clock.h"

#include <gtest/gtest.h>

namespace timekeeper {
namespace {

TEST(TestClockTest, Assignment) {
  TestClock clock;

  zx::time t1(42);

  clock.Set(t1);

  auto t2 = clock.MonotonicNow();

  EXPECT_EQ(t1, t2);

  time_utc t3;

  EXPECT_EQ(ZX_OK, clock.UtcNow(&t3));

  EXPECT_EQ(t1.get(), t3.get());
}

}  // namespace
}  // namespace timekeeper
