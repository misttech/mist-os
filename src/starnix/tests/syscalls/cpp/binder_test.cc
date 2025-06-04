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

TEST_F(BinderTest, InvalidCommandError) {
  fbl::unique_fd binder = fbl::unique_fd(open(TestPath("binder").c_str(), O_RDWR | O_CLOEXEC));
  ASSERT_TRUE(binder) << strerror(errno);

  uint32_t writebuf[1];
  writebuf[0] = -1;
  struct binder_write_read bwr = {};
  bwr.write_buffer = (binder_uintptr_t)writebuf;
  bwr.write_size = sizeof(uint32_t);
  bwr.write_consumed = 0;

  ASSERT_THAT(ioctl(binder.get(), BINDER_WRITE_READ, &bwr), SyscallFailsWithErrno(EINVAL));

  // The failing command is not consumed.
  EXPECT_EQ(bwr.write_consumed, size_t(0));
}

TEST_F(BinderTest, ValidThenInvalidCommand) {
  fbl::unique_fd binder = fbl::unique_fd(open(TestPath("binder").c_str(), O_RDWR | O_CLOEXEC));
  ASSERT_TRUE(binder) << strerror(errno);

  uint32_t writebuf[2];
  writebuf[0] = BC_ENTER_LOOPER;
  writebuf[1] = -1;
  struct binder_write_read bwr = {};
  bwr.write_buffer = (binder_uintptr_t)writebuf;
  bwr.write_size = 2 * sizeof(uint32_t);
  bwr.write_consumed = 0;

  ASSERT_THAT(ioctl(binder.get(), BINDER_WRITE_READ, &bwr), SyscallFailsWithErrno(EINVAL));

  // The first command is consumed.
  EXPECT_EQ(bwr.write_consumed, sizeof(uint32_t));
}

TEST_F(BinderTest, IgnoreAlreadyConsumed) {
  fbl::unique_fd binder = fbl::unique_fd(open(TestPath("binder").c_str(), O_RDWR | O_CLOEXEC));
  ASSERT_TRUE(binder) << strerror(errno);

  uint32_t writebuf[2];
  writebuf[0] = -1;
  writebuf[1] = BC_ENTER_LOOPER;
  struct binder_write_read bwr = {};
  bwr.write_buffer = (binder_uintptr_t)writebuf;
  bwr.write_size = 2 * sizeof(uint32_t);
  bwr.write_consumed = sizeof(uint32_t);

  ASSERT_THAT(ioctl(binder.get(), BINDER_WRITE_READ, &bwr), SyscallSucceeds());

  EXPECT_EQ(bwr.write_consumed, 2 * sizeof(uint32_t));
  EXPECT_EQ(bwr.write_size, 2 * sizeof(uint32_t));

  // We can reuse the structure for the next write, no command will be executed.
  ASSERT_THAT(ioctl(binder.get(), BINDER_WRITE_READ, &bwr), SyscallSucceeds());
}

}  // namespace
