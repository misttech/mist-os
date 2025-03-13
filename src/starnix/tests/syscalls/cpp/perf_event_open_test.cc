// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <stdint.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#include <gtest/gtest.h>

#include "test_helper.h"

namespace {

// See https://man7.org/linux/man-pages/man2/perf_event_open.2.html
struct perf_event_attr {
  int32_t type;
  int32_t size;
  int64_t config;
  // TODO(https://fxbug.dev/394960158): Fill in the rest.
};

// Write wrapper because there isn't one per the man7 page.
long sys_perf_event_open(perf_event_attr *attr, int32_t pid, int32_t cpu, int group_fd,
                         unsigned long flags) {
  return syscall(__NR_perf_event_open, attr, pid, cpu, group_fd, flags);
}

TEST(PerfEventOpenTest, ValidInputsSucceed) {
  // All below are valid inputs.
  perf_event_attr attr = {0, 0, 0};
  int32_t pid = 40;
  int32_t cpu = 0;
  int group_fd = 0;
  long flags = 0;

  // The file descriptor value that is returned is not guaranteed to be a specific number.
  // Just check that's not -1 (error).
  // TODO(https://fxbug.dev/394960158): Change this test when we have something better
  // to test.
  if (test_helper::HasSysAdmin()) {
    EXPECT_NE(sys_perf_event_open(&attr, pid, cpu, group_fd, flags), -1);
  }
}

TEST(PerfEventOpenTest, InvalidPidAndCpuFails) {
  perf_event_attr attr = {0, 0, 0};  // Doesn't matter
  int32_t pid = -1;                  // Invalid
  int32_t cpu = -1;                  // Invalid
  int group_fd = 0;                  // Doesn't matter
  long flags = 0;                    // Doesn't matter

  if (test_helper::HasSysAdmin()) {
    EXPECT_THAT(sys_perf_event_open(&attr, pid, cpu, group_fd, flags),
                SyscallFailsWithErrno(EINVAL));
  }
}

}  // namespace
