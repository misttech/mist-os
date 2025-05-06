// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <unistd.h>

#include <string>

#include <gtest/gtest.h>

#include "src/starnix/tests/selinux/userspace/util.h"

namespace {

// Pipes receive the creating task's context, with no transitions applied.
TEST(PipeTest, LabeledFromTask) {
  auto scoped_current_task =
      ScopedTaskAttrResetter::SetTaskAttr("current", "test_u:test_r:pipe_test_t:s0");

  int pipe_after_policy[2];
  EXPECT_THAT(pipe(pipe_after_policy), SyscallSucceeds());

  EXPECT_THAT(GetLabel(pipe_after_policy[0]), "test_u:test_r:pipe_test_t:s0");
}

int g_before_policy_pipe = -1;

// Pipes created prior to policy load behave the same as those created after policy-load:
// No `type_transition` rules are applied to them, and since all tasks prior to policy load have the
// "kernel" SID, pre-policy pipes will always receive that SID as well.
TEST(PipeTest, BeforePolicyReceivesKernelContext) {
  ASSERT_THAT(ReadTaskAttr("current"), IsOk("system_u:unconfined_r:unconfined_t:s0"));

  EXPECT_THAT(GetLabel(g_before_policy_pipe), "system_u:unconfined_r:unconfined_t:s0");
}

}  // namespace

extern std::string DoPrePolicyLoadWork() {
  // Create a pipe prior to policy load, to allow the test to validate the post-policy label.
  int pipe_before_policy[2];
  EXPECT_THAT(pipe(pipe_before_policy), SyscallSucceeds());
  g_before_policy_pipe = pipe_before_policy[0];

  return "pipe_policy.pp";
}
