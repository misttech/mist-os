// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fit/defer.h>
#include <sys/mount.h>

#include <cstring>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "src/starnix/tests/selinux/userspace/util.h"

/// Returns the path to the policy that should be loaded for use by the test-suite.
/// This hook may also perform pre-policy-load work, e.g. creating kernel objects for later
/// validation by tests.
extern std::string DoPrePolicyLoadWork();

namespace {

void LoadPolicy(const std::string& name) {
  // Ensure that no previous policy has been loaded.
  auto previous_policy = ReadFile("/sys/fs/selinux/policy");
  ASSERT_EQ(previous_policy, fit::error(EINVAL));

  // Load the specified policy from the policy data directory.
  auto policy_path = "data/policies/" + name;
  auto binary_policy = ReadFile(policy_path);
  ASSERT_TRUE(binary_policy.is_ok()) << "Read of policy at " << policy_path
                                     << " failed: " << strerror(binary_policy.error_value());
  auto result = WriteExistingFile("/sys/fs/selinux/load", binary_policy.value());
  ASSERT_TRUE(result.is_ok()) << "Load of policy from " << policy_path
                              << " failed: " << strerror(result.error_value());
}

void PrepareTestEnvironment() {
  EXPECT_THAT(mkdir("/proc", 0755), SyscallSucceeds());
  EXPECT_THAT(mkdir("/sys", 0755), SyscallSucceeds());
  EXPECT_THAT(mkdir("/tmp", 0755), SyscallSucceeds());
  EXPECT_THAT(mount("proc", "/proc", "proc", MS_NOEXEC | MS_NOSUID | MS_NODEV, 0),
              SyscallSucceeds());
  EXPECT_THAT(mount("sysfs", "/sys", "sysfs", MS_NOEXEC | MS_NOSUID | MS_NODEV, 0),
              SyscallSucceeds());
  EXPECT_THAT(mount("selinuxfs", "/sys/fs/selinux", "selinuxfs", MS_NOEXEC | MS_NOSUID, nullptr),
              SyscallSucceeds());
  EXPECT_THAT(mount("tmpfs", "/tmp", "tmpfs", MS_RELATIME, nullptr), SyscallSucceeds());

  auto policy_path = DoPrePolicyLoadWork();
  LoadPolicy(policy_path);
}

}  // namespace

int main(int argc, char** argv) {
  ::testing::InitGoogleTest(&argc, argv);

  PrepareTestEnvironment();

  return RUN_ALL_TESTS();
}
