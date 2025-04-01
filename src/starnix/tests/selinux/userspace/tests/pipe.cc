// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <unistd.h>

#include <string>

#include <gtest/gtest.h>

#include "src/starnix/tests/selinux/userspace/util.h"

// Under the default policy, a pipe is labeled with the label of its creating process.
TEST(PolicyLoadTest, Pipes) {
  int pipe_before_policy[2];
  EXPECT_THAT(pipe(pipe_before_policy), SyscallSucceeds());
  LoadPolicy("minimal_policy.pp");

  EXPECT_THAT(GetLabel(pipe_before_policy[0]), "system_u:unconfined_r:unconfined_t:s0");

  WriteContents("/proc/thread-self/attr/current", "system_u:unconfined_r:unconfined_t:s0");

  int pipe_after_policy[2];
  EXPECT_THAT(pipe(pipe_after_policy), SyscallSucceeds());

  EXPECT_THAT(GetLabel(pipe_after_policy[0]), "system_u:unconfined_r:unconfined_t:s0");
}
