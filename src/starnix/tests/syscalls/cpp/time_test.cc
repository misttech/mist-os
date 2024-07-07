// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <sys/time.h>

#include "src/starnix/tests/syscalls/cpp/syscall_matchers.h"

TEST(TimeTest, ClockGetResMonotonic) {
  clockid_t clockid = CLOCK_MONOTONIC;
  struct timespec tp;
  ASSERT_THAT(clock_getres(clockid, &tp), SyscallSucceeds());
  ASSERT_EQ(tp.tv_sec, 0);
  ASSERT_EQ(tp.tv_nsec, 1);
}

TEST(TimeTest, ClockGetResRealTime) {
  clockid_t clockid = CLOCK_REALTIME;
  struct timespec tp;
  ASSERT_THAT(clock_getres(clockid, &tp), SyscallSucceeds());
  ASSERT_EQ(tp.tv_sec, 0);
  ASSERT_EQ(tp.tv_nsec, 1);
}

TEST(TimeTest, ClockGetResSyscallFail) {
  // Setting clockid to 15 as it will cause an error to be thrown.
  // The vDSO will check if the clockid is valid using the function is_valid_cpu_clock.
  // Since it isn't valid, the vDSO throws an error.
  clockid_t clockid = 15;
  struct timespec tp;
  tp.tv_nsec = 0;
  ASSERT_THAT(clock_getres(clockid, &tp), SyscallFails());
}

// TODO(https://fxbug.dev/350763590) remove no_sanitize attribute and diagnostic suppression
TEST(TimeTest, GetTimeOfDayNullTvSomeTz) __attribute__((no_sanitize("nonnull-attribute"))) {
  struct timezone tz;
// glibc adds nonnull attribute to the tv argument in getttimeofday.
// gettimeofday, however, does allow the tv argument to be NULL.
// To test that the vdso gettimeofday function allows tv to be NULL, the nonnull warning is
// temporarily disabled.
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wnonnull"
  ASSERT_THAT(gettimeofday(nullptr, &tz), SyscallSucceeds());
#pragma GCC diagnostic pop
}

TEST(TimeTest, GetTimeOfDaySomeTvNullTz) {
  struct timeval tv;
  ASSERT_THAT(gettimeofday(&tv, nullptr), SyscallSucceeds());
}

TEST(TimeTest, GetTimeOfDaySomeTvSomeTz) {
  struct timeval tv;
  struct timezone tz;
  ASSERT_THAT(gettimeofday(&tv, &tz), SyscallSucceeds());
}

// TODO(https://fxbug.dev/350763590) remove no_sanitize attribute and diagnostic suppression
TEST(TimeTest, GetTimeOfDayNullTvNullTz) __attribute__((no_sanitize("nonnull-attribute"))) {
// glibc adds nonnull attribute to the tv argument in getttimeofday.
// gettimeofday, however, does allow the tv argument to be NULL.
// To test that the vdso gettimeofday function allows tv to be NULL, the nonnull warning is
// temporarily disabled.
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wnonnull"
  ASSERT_THAT(gettimeofday(nullptr, nullptr), SyscallSucceeds());
#pragma GCC diagnostic pop
}
