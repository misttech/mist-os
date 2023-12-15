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
    EXPECT_GT(size_read, 0ul);
  } while (strstr(message, "Hello from the dev/kmsg test") == nullptr);
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
