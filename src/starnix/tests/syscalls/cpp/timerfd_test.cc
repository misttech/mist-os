// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <stdint.h>
#include <sys/poll.h>
#include <sys/timerfd.h>
#include <time.h>
#include <unistd.h>

#include <chrono>
#include <ctime>
#include <thread>

#include <gtest/gtest.h>
#include <linux/capability.h>

#include "src/starnix/tests/syscalls/cpp/test_helper.h"

void run_timerfd_test(clockid_t clock_id) {
  timespec begin = {};
  ASSERT_EQ(0, clock_gettime(CLOCK_REALTIME, &begin));

  int fd = timerfd_create(clock_id, 0);
  ASSERT_NE(-1, fd) << errno;

  // Test timer 1 second in the future.
  struct itimerspec its = {};
  its.it_value = begin;
  its.it_value.tv_sec += 1;
  EXPECT_EQ(0, timerfd_settime(fd, TFD_TIMER_ABSTIME, &its, nullptr));

  // Test polling on the timer expiration signal.
  pollfd pfd = {.fd = fd, .events = POLLIN};
  ASSERT_EQ(poll(&pfd, 1, -1), 1);
  EXPECT_EQ(pfd.revents, POLLIN);

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

  // Update the time should clear the previous signal.
  ASSERT_EQ(0, clock_gettime(CLOCK_REALTIME, &begin));
  its.it_value = begin;
  its.it_value.tv_sec += 1;
  EXPECT_EQ(0, timerfd_settime(fd, TFD_TIMER_ABSTIME, &its, nullptr));
  std::this_thread::sleep_for(std::chrono::seconds(2));
  its.it_value.tv_sec += 2;
  EXPECT_EQ(0, timerfd_settime(fd, TFD_TIMER_ABSTIME, &its, nullptr));
  ASSERT_EQ(poll(&pfd, 1, 0), 0);
  ASSERT_EQ(poll(&pfd, 1, -1), 1);

  // Stop the timer should clear the previous signal.
  ASSERT_EQ(0, clock_gettime(CLOCK_REALTIME, &begin));
  its.it_value = begin;
  its.it_value.tv_sec += 1;
  EXPECT_EQ(0, timerfd_settime(fd, TFD_TIMER_ABSTIME, &its, nullptr));
  std::this_thread::sleep_for(std::chrono::seconds(2));
  struct itimerspec itempty = {};
  EXPECT_EQ(0, timerfd_settime(fd, TFD_TIMER_ABSTIME, &itempty, nullptr));
  ASSERT_EQ(poll(&pfd, 1, 0), 0);

  close(fd);
}

TEST(TimerFD, RealtimeAbsolute) { run_timerfd_test(CLOCK_REALTIME); }

TEST(TimerFD, RealtimeAlarm) {
  // The CAP_WAKE_ALARM capability is required to create a CLOCK_REALTIME_ALARM timer. If the test
  // process does not have the capability, the timerfd_create call should fail.
  if (test_helper::HasCapability(CAP_WAKE_ALARM)) {
    run_timerfd_test(CLOCK_REALTIME_ALARM);
  } else {
    int fd = timerfd_create(CLOCK_REALTIME_ALARM, 0);
    ASSERT_EQ(-1, fd) << errno;
  }
}
