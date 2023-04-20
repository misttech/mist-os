// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <stdint.h>
#include <sys/timerfd.h>
#include <time.h>
#include <unistd.h>

#include <gtest/gtest.h>

TEST(TimerFD, RealtimeAbsolute) {
  timespec begin = {};
  ASSERT_EQ(0, clock_gettime(CLOCK_REALTIME, &begin));

  // int fd = timerfd_create(CLOCK_REALTIME, 0);
  int fd = timerfd_create(CLOCK_REALTIME, 0);
  ASSERT_NE(-1, fd) << errno;

  // Test timer 1 second in the future.
  struct itimerspec its = {};
  its.it_value = begin;
  its.it_value.tv_sec += 1;
  EXPECT_EQ(0, timerfd_settime(fd, TFD_TIMER_ABSTIME, &its, nullptr));

  uint64_t val = 0;
  EXPECT_EQ(ssize_t(sizeof(val)), read(fd, &val, sizeof(val))) << errno;
  EXPECT_EQ(1u, val);  // Timer went off one time.

  // Elapsed time should be at least one second in the future.
  timespec end = {};
  ASSERT_EQ(0, clock_gettime(CLOCK_REALTIME, &end));
  EXPECT_LT(begin.tv_sec, end.tv_sec);

  // Set the timer in the past.
  its.it_value.tv_sec = begin.tv_sec - 10;
  EXPECT_EQ(0, timerfd_settime(fd, TFD_TIMER_ABSTIME, &its, nullptr));

  // Should be called immediately. That's hard to check in a non-flaky way so this just checks that
  // it went off at all.
  val = 0;
  EXPECT_EQ(ssize_t(sizeof(val)), read(fd, &val, sizeof(val))) << errno;
  EXPECT_EQ(1u, val);  // Timer went off one time.

  close(fd);
}
