// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/starnix/tests/syscalls/syscall_matchers.h"

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
  // 15 isn't the clockid of any of the clocks that the vDSO can use, so the syscall is called.
  // 15 is an invalid dynamic clockid. The syscall will check if the clockid is valid using the
  // function is_valid_cpu_clock. Since it isn't valid, the syscall throws an error.
  clockid_t clockid = 15;
  struct timespec tp;
  tp.tv_nsec = 0;
  ASSERT_THAT(clock_getres(clockid, &tp), SyscallFails());
}
