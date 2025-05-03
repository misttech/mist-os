// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "src/starnix/tests/selinux/userspace/util.h"

extern std::string DoPrePolicyLoadWork() { return "minimal_policy.pp"; }

TEST(PolicyLoadTest, TasksUseKernelSid) {
  // All processes created prior to policy loading are labeled with the kernel SID.
  EXPECT_THAT(ReadTaskAttr("current"), IsOk("system_u:unconfined_r:unconfined_t:s0"));
}
