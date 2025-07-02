// Copyright 2025 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <stdint.h>
#include <sys/ioctl.h>
#include <sys/mount.h>
#include <unistd.h>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>
#include <linux/android/binder.h>

#include "src/starnix/tests/selinux/userspace/util.h"
#include "src/starnix/tests/syscalls/cpp/syscall_matchers.h"
#include "src/starnix/tests/syscalls/cpp/test_helper.h"

extern std::string DoPrePolicyLoadWork() { return "binder.pp"; }

namespace {

class BinderTest : public ::testing::Test {
 public:
  void SetUp() override {
    EXPECT_THAT(mount("binder", "/tmp", "binder", 0, nullptr), SyscallSucceeds());
  }
  std::string TestPath(const char *path) const { return std::string("/tmp/") + path; }
};

TEST_F(BinderTest, OpenBinderNoTestDomain) {
  auto enforce = ScopedEnforcement::SetEnforcing();
  fbl::unique_fd binder = fbl::unique_fd(open("/tmp/binder", O_RDWR | O_CLOEXEC));
  EXPECT_TRUE(binder) << strerror(errno);
}

TEST_F(BinderTest, OpenBinderWithTestDomain) {
  auto enforce = ScopedEnforcement::SetEnforcing();
  ASSERT_TRUE(RunSubprocessAs("test_u:test_r:binder_open_test_t:s0", [&] {
    fbl::unique_fd binder = fbl::unique_fd(open("/tmp/binder", O_RDWR | O_CLOEXEC));
    EXPECT_TRUE(binder) << strerror(errno);
  }));
}

}  // namespace
