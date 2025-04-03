// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/inotify.h>
#include <sys/signalfd.h>
#include <sys/syscall.h>
#include <sys/timerfd.h>
#include <unistd.h>

#include <string>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>
#include <linux/userfaultfd.h>

#include "src/starnix/tests/selinux/userspace/util.h"

namespace {

TEST(AnonInodeTest, EventFdIsUnlabeled) {
  LoadPolicy("anon_inode_policy.pp");

  fbl::unique_fd fd(eventfd(0, 0));
  ASSERT_TRUE(fd.is_valid());

  EXPECT_EQ(GetLabel(fd.get()), fit::error(ENOTSUP));
}

TEST(AnonInodeTest, PrivateFdIsUnchecked) {
  LoadPolicy("anon_inode_policy.pp");

  auto enforce = ScopedEnforcement::SetEnforcing();

  // Create an eventfd within a test domain, then validate whether the FD is usable from a set
  // of test domains with differing levels of access.
  ASSERT_TRUE(RunSubprocessAs("test_u:test_r:anon_inode_test_t:s0", [] {
    fbl::unique_fd fd(eventfd(0, 0));
    ASSERT_TRUE(fd.is_valid());
    auto fd_label = GetLabel(fd.get());

    // Ensure that `fd` is of an un-labeled, aka "private", kind.
    ASSERT_EQ(fd_label, fit::error(ENOTSUP));

    uint64_t event_buf = 1;

    ASSERT_TRUE(RunSubprocessAs("test_u:test_r:anon_inode_use_fd_and_perms:s0", [&] {
      EXPECT_THAT(write(fd.get(), &event_buf, sizeof(event_buf)), SyscallSucceeds())
          << "Domain granted FD-use and permissions should have access";
    }));
    ASSERT_TRUE(RunSubprocessAs("test_u:test_r:anon_inode_use_fd_no_perms:s0", [&] {
      EXPECT_THAT(write(fd.get(), &event_buf, sizeof(event_buf)), SyscallSucceeds())
          << "Domain granted FD-use but no file node permissions should have access";
    }));
    ASSERT_TRUE(RunSubprocessAs("test_u:test_r:anon_inode_no_use_fd:s0", [&] {
      EXPECT_THAT(write(fd.get(), &event_buf, sizeof(event_buf)), SyscallFailsWithErrno(EACCES))
          << "Domain not granted FD-use should not have access";
    }));
  }));
}

TEST(AnonInodeTest, TmpFileHasLabel) {
  LoadPolicy("anon_inode_policy.pp");

  constexpr char kTmpPath[] = "/tmp";
  fbl::unique_fd fd(open(kTmpPath, O_RDWR | O_TMPFILE));
  ASSERT_TRUE(fd.is_valid());

  EXPECT_EQ(GetLabel(fd.get()), fit::ok());
}

TEST(AnonInodeTest, UserfaultFdHasLabel) {
  LoadPolicy("anon_inode_policy.pp");

  fbl::unique_fd fd(static_cast<int>(syscall(SYS_userfaultfd, O_CLOEXEC)));
  ASSERT_TRUE(fd.is_valid());

  EXPECT_EQ(GetLabel(fd.get()),
            fit::ok("system_u:object_r:anon_inode_unconfined_userfaultfd_t:s0"));
}

TEST(AnonInodeTest, EpollIsUnlabeled) {
  LoadPolicy("anon_inode_policy.pp");

  fbl::unique_fd fd(epoll_create1(0));
  ASSERT_TRUE(fd.is_valid());

  EXPECT_EQ(GetLabel(fd.get()), fit::error(ENOTSUP));
}

TEST(AnonInodeTest, InotifyIsUnlabeled) {
  LoadPolicy("anon_inode_policy.pp");

  fbl::unique_fd fd(inotify_init());
  ASSERT_TRUE(fd.is_valid());

  EXPECT_EQ(GetLabel(fd.get()), fit::error(ENOTSUP));
}

TEST(AnonInodeTest, PidFdIsUnlabeled) {
  LoadPolicy("anon_inode_policy.pp");

  fbl::unique_fd fd(static_cast<int>(syscall(SYS_pidfd_open, getpid(), 0)));
  ASSERT_TRUE(fd.is_valid());

  EXPECT_EQ(GetLabel(fd.get()), fit::error(ENOTSUP));
}

TEST(AnonInodeTest, TimerFdIsUnlabeled) {
  LoadPolicy("anon_inode_policy.pp");

  fbl::unique_fd fd(timerfd_create(CLOCK_MONOTONIC, 0));
  ASSERT_TRUE(fd.is_valid());

  EXPECT_EQ(GetLabel(fd.get()), fit::error(ENOTSUP));
}

TEST(AnonInodeTest, SignalFdIsUnlabeled) {
  LoadPolicy("anon_inode_policy.pp");

  sigset_t signals;
  sigemptyset(&signals);
  fbl::unique_fd fd(signalfd(-1, &signals, SFD_CLOEXEC));
  ASSERT_TRUE(fd.is_valid());

  EXPECT_EQ(GetLabel(fd.get()), fit::error(ENOTSUP));
}

}  // namespace
