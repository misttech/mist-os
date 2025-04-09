// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <sys/syscall.h>
#include <unistd.h>

#include <gtest/gtest.h>
#include <linux/sched.h>

#include "src/starnix/tests/syscalls/cpp/test_helper.h"

TEST(ThreadGroupLifeCycleTest, ErroneousClone) {
  if (!test_helper::IsStarnix()) {
    // This is testing for a starnix bug. The behavior is different on Linux
    return;
  }
  struct clone_args ca;
  memset(&ca, 0, sizeof(ca));
  ca.flags = CLONE_CHILD_SETTID;
  ASSERT_EQ(-1, syscall(__NR_clone3, &ca, sizeof(struct clone_args)));
  ASSERT_EQ(EFAULT, errno);

  // Fork to ensure starnix iterate over current children.
  pid_t fork_result = fork();
  ASSERT_GE(fork_result, 0);
  if (fork_result == 0) {
    _Exit(0);
  }
}
