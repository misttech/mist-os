// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <sys/mount.h>

#include <gtest/gtest.h>

#include "src/starnix/tests/selinux/userspace/util.h"

namespace {

// Perform one-time initialization of the test system.
void PrepareTestEnvironment() {
  ASSERT_THAT(mkdir("/proc", 0755), SyscallSucceeds());
  ASSERT_THAT(mkdir("/sys", 0755), SyscallSucceeds());
  ASSERT_THAT(mkdir("/tmp", 0755), SyscallSucceeds());
  ASSERT_THAT(mount("proc", "/proc", "proc", MS_NOEXEC | MS_NOSUID | MS_NODEV, 0),
              SyscallSucceeds());
  ASSERT_THAT(mount("sysfs", "/sys", "sysfs", MS_NOEXEC | MS_NOSUID | MS_NODEV, 0),
              SyscallSucceeds());
  ASSERT_THAT(mount("selinuxfs", "/sys/fs/selinux", "selinuxfs", MS_NOEXEC | MS_NOSUID, nullptr),
              SyscallSucceeds());
  ASSERT_THAT(mount("tmpfs", "/tmp", "tmpfs", MS_RELATIME, nullptr), SyscallSucceeds());
}

class UserspaceTestEnvironment : public ::testing::Environment {
 public:
  void SetUp() override {
    PrepareTestEnvironment();

    // gTest is documented as treating `Environment::SetUp` fatal failures as fatal, but does not
    // appear to actually do so, so we manually terminate the attempt on setup failures.
    if (::testing::Test::HasFailure()) {
      fprintf(stderr, "Test environment setup failed => failing all tests.\n");
      fflush(stdout);
      fflush(stderr);
      _exit(1);
    }
  }
};

}  // namespace

int main(int argc, char** argv) {
  ::testing::InitGoogleTest(&argc, argv);

  // Set up gTest to perform test environment setup at-most-once.
  GTEST_FLAG_SET(recreate_environments_when_repeating, false);
  ::testing::AddGlobalTestEnvironment(new UserspaceTestEnvironment);

  return RUN_ALL_TESTS();
}
