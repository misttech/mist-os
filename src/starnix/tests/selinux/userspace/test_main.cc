// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <sys/mount.h>

#include <gtest/gtest.h>

#include "src/starnix/tests/selinux/userspace/util.h"

extern void RunTest();

void PrepareTestEnvironment() {
  EXPECT_THAT(mkdir("/sys", 0755), SyscallSucceeds());
  EXPECT_THAT(mkdir("/proc", 0755), SyscallSucceeds());
  EXPECT_THAT(mount("proc", "/proc", "proc", MS_NOEXEC | MS_NOSUID | MS_NODEV, 0),
              SyscallSucceeds());
  EXPECT_THAT(mount("sysfs", "/sys", "sysfs", MS_NOEXEC | MS_NOSUID | MS_NODEV, 0),
              SyscallSucceeds());
  EXPECT_THAT(mount("selinuxfs", "/sys/fs/selinux", "selinuxfs", MS_NOEXEC | MS_NOSUID, nullptr),
              SyscallSucceeds());
}

// Wrap running the test function in a gtest test to get access to the nice EXPECT/ASSERT magic.
TEST(StarnixUserspaceTest, TestWrapper) {
  PrepareTestEnvironment();
  RunTest();
}

int main(int argc, char** argv) {
  ::testing::InitGoogleTest(&argc, argv);

  return RUN_ALL_TESTS();
}
