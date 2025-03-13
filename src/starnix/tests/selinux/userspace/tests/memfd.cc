// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <sys/mman.h>
#include <sys/xattr.h>
#include <unistd.h>

#include <string>

#include <gtest/gtest.h>

#include "src/starnix/tests/selinux/userspace/util.h"

TEST(PolicyLoadTest, MemFdsRetrospectivelyLabeledOnPolicyLoad) {
  int before_policy_fd;
  EXPECT_THAT((before_policy_fd = memfd_create("test", 0)), SyscallSucceeds());

  LoadPolicy("memfd_transition.pp");
  EXPECT_THAT(before_policy_fd, FdIsLabeled("system_u:object_r:transition_t:s0"));

  int fd;
  EXPECT_THAT((fd = memfd_create("test", 0)), SyscallSucceeds());
  EXPECT_THAT(fd, FdIsLabeled("system_u:object_r:transition_t:s0"));
}
