// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <sys/mount.h>
#include <sys/syscall.h>

#include <cerrno>
#include <csignal>
#include <ctime>

#include <gtest/gtest.h>
#include <linux/capability.h>

#include "src/starnix/tests/syscalls/cpp/test_helper.h"

TEST(Timers, NoWakeAlarmCap) {
  test_helper::UnsetCapability(CAP_WAKE_ALARM);
  timer_t timer_id;
  struct sigevent sev = {};
  sev.sigev_notify = SIGEV_NONE;

  EXPECT_EQ(-1, timer_create(CLOCK_BOOTTIME_ALARM, &sev, &timer_id));
  EXPECT_EQ(errno, EPERM);

  EXPECT_EQ(-1, timer_create(CLOCK_REALTIME_ALARM, &sev, &timer_id));
  EXPECT_EQ(errno, EPERM);
}

TEST(Timers, RealtimeAlarm) {
  if (!test_helper::HasCapability(CAP_WAKE_ALARM)) {
    GTEST_SKIP()
        << "The CAP_WAKE_ALARM capability is required to create a CLOCK_REALTIME_ALARM timer.";
  }
  timespec begin = {};
  ASSERT_EQ(0, clock_gettime(CLOCK_REALTIME, &begin));

  timer_t timer_id;
  struct sigevent sev = {};
  sev.sigev_notify = SIGEV_NONE;
  SAFE_SYSCALL(timer_create(CLOCK_REALTIME_ALARM, &sev, &timer_id));

  // Test timer 1 second in the future.
  struct itimerspec its = {};
  its.it_value = begin;
  its.it_value.tv_sec += 1;
  SAFE_SYSCALL(timer_settime(timer_id, TIMER_ABSTIME, &its, nullptr));

  struct timespec sleep_t = {
      .tv_sec = 1,
      .tv_nsec = 5000,
  };
  nanosleep(&sleep_t, nullptr);

  // The timer should count down to 0 and stop since the interval is zero. No overruns should be
  // counted.
  EXPECT_EQ(0, timer_gettime(timer_id, &its));
  EXPECT_EQ(0, its.it_value.tv_sec);
  EXPECT_EQ(0, its.it_value.tv_nsec);
  EXPECT_EQ(0, timer_getoverrun(timer_id));

  SAFE_SYSCALL(timer_delete(timer_id));
}
