// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <sys/mman.h>
#include <sys/xattr.h>
#include <unistd.h>

#include <string>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "src/starnix/tests/selinux/userspace/util.h"

namespace {

int g_before_policy_fd = -1;

TEST(PolicyLoadTest, MemFdsRetrospectivelyLabeledOnPolicyLoad) {
  EXPECT_THAT(GetLabel(g_before_policy_fd), "system_u:object_r:transition_t:s0");

  int fd;
  EXPECT_THAT((fd = memfd_create("test", 0)), SyscallSucceeds());
  EXPECT_THAT(GetLabel(fd), "system_u:object_r:transition_t:s0");
}

}  // namespace

extern std::string DoPrePolicyLoadWork() {
  // Create a memfd prior to policy load, to allow the test to validate the post-policy label.
  EXPECT_THAT((g_before_policy_fd = memfd_create("test", 0)), SyscallSucceeds());

  return "memfd_transition.pp";
}
