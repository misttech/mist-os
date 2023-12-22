// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <poll.h>
#include <string.h>
#include <sys/klog.h>
#include <unistd.h>

#include <string>

#include <gtest/gtest.h>

class SyslogNonRootTest : public ::testing::Test {
 public:
  //  This test is intended to not be executed as root.
  void SetUp() override { ASSERT_NE(getuid(), 0u); }
};

TEST_F(SyslogNonRootTest, DevKmsg) {
  EXPECT_LT(open("/dev/kmsg", O_RDONLY), 0);
  EXPECT_EQ(errno, EPERM);

  EXPECT_LT(open("/dev/kmsg", O_WRONLY), 0);
  EXPECT_EQ(errno, EPERM);

  EXPECT_LT(open("/dev/kmsg", O_RDWR), 0);
  EXPECT_EQ(errno, EPERM);
}

TEST_F(SyslogNonRootTest, ProcKmsg) {
  EXPECT_LT(open("/proc/kmsg", O_RDONLY), 0);
  EXPECT_EQ(errno, EACCES);

  EXPECT_LT(open("/proc/kmsg", O_WRONLY), 0);
  EXPECT_EQ(errno, EACCES);

  EXPECT_LT(open("/proc/kmsg", O_RDWR), 0);
  EXPECT_EQ(errno, EACCES);
}

TEST_F(SyslogNonRootTest, Syslog) {
  EXPECT_LT(klogctl(2 /* SYSLOG_ACTION_READ */, NULL, 0), 0);
  EXPECT_EQ(errno, EPERM);

  EXPECT_LT(klogctl(3 /* SYSLOG_ACTION_READ_ALL */, NULL, 0), 0);
  EXPECT_EQ(errno, EPERM);

  EXPECT_LT(klogctl(4 /* SYSLOG_ACTION_READ_CLEAR */, NULL, 0), 0);
  EXPECT_EQ(errno, EPERM);

  EXPECT_LT(klogctl(5 /* SYSLOG_ACTION_CLEAR */, NULL, 0), 0);
  EXPECT_EQ(errno, EPERM);

  EXPECT_LT(klogctl(9 /* SYSLOG_ACTION_SIZE_UNREAD */, NULL, 0), 0);
  EXPECT_EQ(errno, EPERM);

  EXPECT_LT(klogctl(10 /* SYSLOG_ACTION_SIZE_BUFFER */, NULL, 0), 0);
  EXPECT_EQ(errno, EPERM);
}
