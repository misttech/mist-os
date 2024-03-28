// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <zircon/time.h>

#include <gtest/gtest.h>

#include "test-thread.h"

namespace {

using Duration = sched::Duration;
using Time = sched::Time;

TEST(ThreadBaseTests, Start) {
  {
    TestThread thread{{Period(10), Capacity(5)}, Start(0)};
    EXPECT_EQ(Time{0}, thread.start());
  }
  {
    TestThread thread{{Period(100), Capacity(10)}, Start(10)};
    EXPECT_EQ(Time{10}, thread.start());
  }
}

TEST(ThreadBaseTests, Finish) {
  {
    TestThread thread{{Period(10), Capacity(5)}, Start(0)};
    EXPECT_EQ(Time{10}, thread.finish());  // start + period
  }
  {
    TestThread thread{{Period(100), Capacity(10)}, Start(10)};
    EXPECT_EQ(Time{110}, thread.finish());  // start + period
  }
}

TEST(ThreadBaseTests, Tick) {
  TestThread thread{{Period(10), Capacity(5)}, Start(0)};
  EXPECT_EQ(Capacity(5), thread.firm_capacity());
  EXPECT_EQ(Duration{0}, thread.time_slice_used());
  EXPECT_EQ(Duration{5}, thread.time_slice_remaining());
  EXPECT_FALSE(thread.IsExpired(Start(0)));

  // Though would be expired at the end of the period.
  EXPECT_TRUE(thread.IsExpired(Start(10)));

  thread.Tick(Duration{2});
  EXPECT_EQ(Capacity(5), thread.firm_capacity());
  EXPECT_EQ(Duration{2}, thread.time_slice_used());
  EXPECT_EQ(Duration{3}, thread.time_slice_remaining());
  EXPECT_FALSE(thread.IsExpired(Start(0)));

  thread.Tick(Duration{3});
  EXPECT_EQ(Capacity(5), thread.firm_capacity());
  EXPECT_EQ(Duration{5}, thread.time_slice_used());
  EXPECT_EQ(Duration{0}, thread.time_slice_remaining());
  EXPECT_TRUE(thread.IsExpired(Start(0)));
}

TEST(ThreadBaseTests, ReactivateIfExpired) {
  TestThread thread{{Period(10), Capacity(5)}, Start(0)};
  EXPECT_EQ(Time{0}, thread.start());
  EXPECT_EQ(Time{10}, thread.finish());

  // Not expired at t=0, so call should have no effect.
  thread.ReactivateIfExpired(Start(0));
  EXPECT_EQ(Time{0}, thread.start());
  EXPECT_EQ(Time{10}, thread.finish());

  // But indeed expired at t=15.
  thread.ReactivateIfExpired(Start(15));
  EXPECT_EQ(Time{15}, thread.start());
  EXPECT_EQ(Time{25}, thread.finish());
}

}  // namespace
