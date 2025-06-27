// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <poll.h>
#include <sys/klog.h>
#include <unistd.h>

#include <gtest/gtest.h>

namespace {

// See "The symbolic names are defined in the kernel source, but are
// not exported to user space; you will either need to use the numbers,
// or define the names yourself" at syslog(2).
constexpr int SYSLOG_ACTION_READ = 2;
constexpr int SYSLOG_ACTION_READ_ALL = 3;
constexpr int SYSLOG_ACTION_CLEAR = 5;
constexpr int SYSLOG_ACTION_READ_CLEAR = 6;
constexpr int SYSLOG_ACTION_SIZE_UNREAD = 9;
constexpr int SYSLOG_ACTION_SIZE_BUFFER = 10;

class SyslogNonRootTest : public ::testing::Test {
 public:
  //  This test is intended to not be executed as root.
  void SetUp() override { ASSERT_NE(getuid(), 0u); }
};

TEST_F(SyslogNonRootTest, ProcKmsg) {
  EXPECT_LT(open("/proc/kmsg", O_RDONLY), 0);
  EXPECT_EQ(errno, EACCES);

  EXPECT_LT(open("/proc/kmsg", O_WRONLY), 0);
  EXPECT_EQ(errno, EACCES);

  EXPECT_LT(open("/proc/kmsg", O_RDWR), 0);
  EXPECT_EQ(errno, EACCES);
}

TEST_F(SyslogNonRootTest, Syslog) {
  EXPECT_LT(klogctl(SYSLOG_ACTION_READ, nullptr, 0), 0);
  EXPECT_EQ(errno, EPERM);

  EXPECT_LT(klogctl(SYSLOG_ACTION_READ_ALL, nullptr, 0), 0);
  EXPECT_EQ(errno, EPERM);

  EXPECT_LT(klogctl(SYSLOG_ACTION_READ_CLEAR, nullptr, 0), 0);
  EXPECT_EQ(errno, EPERM);

  EXPECT_LT(klogctl(SYSLOG_ACTION_CLEAR, nullptr, 0), 0);
  EXPECT_EQ(errno, EPERM);

  EXPECT_LT(klogctl(SYSLOG_ACTION_SIZE_UNREAD, nullptr, 0), 0);
  EXPECT_EQ(errno, EPERM);

  EXPECT_LT(klogctl(SYSLOG_ACTION_SIZE_BUFFER, nullptr, 0), 0);
  EXPECT_EQ(errno, EPERM);
}

}  // namespace
