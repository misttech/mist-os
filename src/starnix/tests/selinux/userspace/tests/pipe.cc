// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <unistd.h>

#include <string>

#include <gtest/gtest.h>

#include "src/starnix/tests/selinux/userspace/util.h"

namespace {

int g_before_policy_pipe = -1;

// Under the default policy, a pipe is labeled with the label of its creating process.
TEST(PolicyLoadTest, Pipes) {
  EXPECT_THAT(GetLabel(g_before_policy_pipe), "system_u:unconfined_r:unconfined_t:s0");

  ASSERT_EQ(WriteTaskAttr("current", "system_u:unconfined_r:unconfined_t:s0"), fit::ok());

  int pipe_after_policy[2];
  EXPECT_THAT(pipe(pipe_after_policy), SyscallSucceeds());

  EXPECT_THAT(GetLabel(pipe_after_policy[0]), "system_u:unconfined_r:unconfined_t:s0");
}

}  // namespace

extern std::string DoPrePolicyLoadWork() {
  // Create a pipe prior to policy load, to allow the test to validate the post-policy label.
  int pipe_before_policy[2];
  EXPECT_THAT(pipe(pipe_before_policy), SyscallSucceeds());
  g_before_policy_pipe = pipe_before_policy[0];

  return "minimal_policy.pp";
}
