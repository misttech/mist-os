// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <string.h>
#include <sys/klog.h>
#include <unistd.h>

#include <gtest/gtest.h>

class SyslogTest : public ::testing::Test {
 public:
  void SetUp() override {
    if (getuid() != 0) {
      GTEST_SKIP() << "Can only be run as root";
    }
  }
};

TEST_F(SyslogTest, ReadDevKmsg) {
  int kmsg_fd = open("/dev/kmsg", O_RDWR);
  if (kmsg_fd < 0) {
    printf("Failed to open /dev/kmsg for writing: %s\n", strerror(errno));
    FAIL();
  }
  dprintf(kmsg_fd, "Hello from the dev/kmsg test\n");

  char message[4096];
  do {
    size_t size_read = read(kmsg_fd, message, sizeof(message));
    ASSERT_GT(size_read, 0ul);
  } while (strstr(message, "Hello from the dev/kmsg test") == nullptr);

  close(kmsg_fd);
}

TEST_F(SyslogTest, SyslogReadAll) {
  int kmsg_fd = open("/dev/kmsg", O_WRONLY);
  if (kmsg_fd < 0) {
    printf("Failed to open /dev/kmsg -> %s\n", strerror(errno));
    FAIL();
  }
  dprintf(kmsg_fd, "Hello from the read-all test\n");
  close(kmsg_fd);

  int size = klogctl(10 /* SYSLOG_ACTION_SIZE_BUFFER */, NULL, 0);
  char* buf = (char*)malloc(size);
  int size_read = klogctl(3 /* SYSLOG_ACTION_READ_ALL */, buf, size);
  if (size_read <= 0) {
    printf("Failed to read: %s\n", strerror(errno));
    FAIL();
  }
  EXPECT_NE(strstr(buf, "Hello from the read-all test"), nullptr);
  free(buf);
}

TEST_F(SyslogTest, Read) {
  int kmsg_fd = open("/dev/kmsg", O_RDWR);
  if (kmsg_fd < 0) {
    printf("Failed to open /dev/kmsg for writing: %s\n", strerror(errno));
    FAIL();
  }

  // Write a first log.
  dprintf(kmsg_fd, "SyslogRead -- first\n");

  // Read that first log we wrote.
  char buf[4096];
  do {
    int size_read = klogctl(2 /* SYSLOG_ACTION_READ */, buf, sizeof(buf));
    ASSERT_GT(size_read, 0);
  } while (strstr(buf, "SyslogRead -- first") == nullptr);

  // Write a second log.
  dprintf(kmsg_fd, "SyslogRead -- second\n");

  // Check that the first log we syslog(READ) from isn't present anymore.
  do {
    std::fill_n(buf, 4096, 0);
    int size_read = klogctl(2 /* SYSLOG_ACTION_READ */, buf, sizeof(buf));
    ASSERT_GT(size_read, 0);
    EXPECT_EQ(strstr(buf, "SyslogRead -- first"), nullptr);
  } while (strstr(buf, "SyslogRead -- second") == nullptr);

  // Check that all logs are present when reading from /dev/kmsg.
  do {
    std::fill_n(buf, 4096, 0);
    size_t size_read = read(kmsg_fd, buf, sizeof(buf));
    ASSERT_GT(size_read, 0ul);
  } while (strstr(buf, "SyslogRead -- first") == nullptr);

  while (strstr(buf, "SyslogRead -- second") == nullptr) {
    size_t size_read = read(kmsg_fd, buf, sizeof(buf));
    ASSERT_GT(size_read, 0ul);
  }

  // Check that all logs are present when reading using SYSLOG_ACTION_READ_ALL
  int size = klogctl(10 /* SYSLOG_ACTION_SIZE_BUFFER */, NULL, 0);
  char* buf_all = (char*)malloc(size);
  int size_read = klogctl(3 /* SYSLOG_ACTION_READ_ALL */, buf_all, size);
  if (size_read <= 0) {
    printf("Failed to read: %s\n", strerror(errno));
    FAIL();
  }
  EXPECT_NE(strstr(buf_all, "SyslogRead -- first"), nullptr);
  EXPECT_NE(strstr(buf_all, "SyslogRead -- second"), nullptr);
  free(buf_all);

  close(kmsg_fd);
}
