// Copyright 2025 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <sys/ioctl.h>
#include <sys/mount.h>
#include <unistd.h>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>
#include <linux/android/binder.h>

#include "src/starnix/tests/syscalls/cpp/syscall_matchers.h"
#include "src/starnix/tests/syscalls/cpp/test_helper.h"

namespace {

static bool skip_binder_tests = false;

class BinderTest : public ::testing::Test {
 public:
  static void SetUpTestSuite() {
    // The unshare() call will isolate the mount namespaces for the running
    // test process. This allows the Linux-based tests to execute syscalls with
    // root permissions, without fear of messing the environment up. While the
    // Starnix tests don't strictly need to unshare, it's beneficial to run the
    // same test binaries on Linux and on Starnix so we can be sure the semantics
    // match. As a side effect, this means that the mounted directories will not
    // be viewable in traditional ways, e.g. ffx component explore.
    // TODO(https://fxbug.dev/317285180) don't skip on baseline
    int rv = unshare(CLONE_NEWNS);
    if (rv == -1 && errno == EPERM) {
      // GTest does not support GTEST_SKIP() from a suite setup, so record that we want to skip
      // every test here and skip in SetUp().
      skip_binder_tests = true;
      return;
    }
    ASSERT_EQ(rv, 0) << "unshare(CLONE_NEWNS) failed: " << strerror(errno) << "(" << errno << ")";
  }

  void SetUp() override {
    if (skip_binder_tests) {
      GTEST_SKIP() << "Permission denied for unshare(CLONE_NEWNS), skipping suite.";
    }

    ASSERT_FALSE(temp_dir_.path().empty());
    if (mount("binder", temp_dir_.path().c_str(), "binder", 0, nullptr) < 0) {
      ASSERT_EQ(errno, ENODEV);
      GTEST_SKIP() << "binderfs is not available, skipping test.";
    }
  }

  std::string TestPath(const char *path) const { return temp_dir_.path() + "/" + path; }

 private:
  test_helper::ScopedTempDir temp_dir_;
};

TEST_F(BinderTest, SetContextMgrWithNull) {
  fbl::unique_fd binder = fbl::unique_fd(open(TestPath("binder").c_str(), O_RDWR | O_CLOEXEC));
  ASSERT_TRUE(binder) << strerror(errno);

  EXPECT_THAT(ioctl(binder.get(), BINDER_SET_CONTEXT_MGR, 0), SyscallSucceeds());
}

}  // namespace
