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

  auto fd_label = GetLabel(fd.get());
  ASSERT_TRUE(fd_label.is_error());

  EXPECT_EQ(fd_label.error_value(), ENOTSUP);
}

TEST(AnonInodeTest, TmpFileHasLabel) {
  LoadPolicy("anon_inode_policy.pp");

  constexpr char kTmpPath[] = "/tmp";
  fbl::unique_fd fd(open(kTmpPath, O_RDWR | O_TMPFILE));
  ASSERT_TRUE(fd.is_valid());

  auto fd_label = GetLabel(fd.get());
  ASSERT_TRUE(fd_label.is_ok());
}

TEST(AnonInodeTest, UserfaultFdHasLabel) {
  LoadPolicy("anon_inode_policy.pp");

  fbl::unique_fd fd(static_cast<int>(syscall(SYS_userfaultfd, O_CLOEXEC)));
  ASSERT_TRUE(fd.is_valid());

  auto fd_label = GetLabel(fd.get());
  ASSERT_TRUE(fd_label.is_ok());

  EXPECT_EQ(fd_label.value(), "system_u:object_r:anon_inode_unconfined_userfaultfd_t:s0");
}

TEST(AnonInodeTest, EpollIsUnlabeled) {
  LoadPolicy("anon_inode_policy.pp");

  fbl::unique_fd fd(epoll_create1(0));
  ASSERT_TRUE(fd.is_valid());

  auto fd_label = GetLabel(fd.get());
  ASSERT_TRUE(fd_label.is_error());

  EXPECT_EQ(fd_label.error_value(), ENOTSUP);
}

TEST(AnonInodeTest, InotifyIsUnlabeled) {
  LoadPolicy("anon_inode_policy.pp");

  fbl::unique_fd fd(inotify_init());
  ASSERT_TRUE(fd.is_valid());

  auto fd_label = GetLabel(fd.get());
  ASSERT_TRUE(fd_label.is_error());

  EXPECT_EQ(fd_label.error_value(), ENOTSUP);
}

TEST(AnonInodeTest, PidFdIsUnlabeled) {
  LoadPolicy("anon_inode_policy.pp");

  fbl::unique_fd fd(static_cast<int>(syscall(SYS_pidfd_open, getpid(), 0)));
  ASSERT_TRUE(fd.is_valid());

  auto fd_label = GetLabel(fd.get());
  ASSERT_TRUE(fd_label.is_error());

  EXPECT_EQ(fd_label.error_value(), ENOTSUP);
}

TEST(AnonInodeTest, TimerFdIsUnlabeled) {
  LoadPolicy("anon_inode_policy.pp");

  fbl::unique_fd fd(timerfd_create(CLOCK_MONOTONIC, 0));
  ASSERT_TRUE(fd.is_valid());

  auto fd1_label = GetLabel(fd.get());
  ASSERT_TRUE(fd1_label.is_error());

  EXPECT_EQ(fd1_label.error_value(), ENOTSUP);
}

}  // namespace
