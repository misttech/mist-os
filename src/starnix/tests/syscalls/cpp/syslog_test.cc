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

class SyslogTest : public ::testing::Test {
 public:
  void SetUp() override {
    // TODO(https://fxbug.dev/317285180) don't skip on baseline
    if (getuid() != 0) {
      GTEST_SKIP() << "Can only be run as root";
    }
  }
};

TEST_F(SyslogTest, ReadDevKmsg) {
  int kmsg_fd = open("/dev/kmsg", O_RDWR);
  if (kmsg_fd < 0) {
    fprintf(stderr, "Failed to open /dev/kmsg for writing: %s\n", strerror(errno));
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
    fprintf(stderr, "Failed to open /dev/kmsg -> %s\n", strerror(errno));
    FAIL();
  }
  dprintf(kmsg_fd, "Hello from the read-all test\n");
  close(kmsg_fd);

  int size = klogctl(10 /* SYSLOG_ACTION_SIZE_BUFFER */, NULL, 0);
  std::string buf;
  buf.resize(size);

  // Logging is an asynchronous process, so we must loop.
  do {
    int size_read =
        klogctl(3 /* SYSLOG_ACTION_READ_ALL */, buf.data(), static_cast<int>(buf.size()));
    if (size_read <= 0) {
      fprintf(stderr, "Failed to read: %s\n", strerror(errno));
      FAIL();
    }
  } while (buf.find("Hello from the read-all test") == std::string::npos);
}

TEST_F(SyslogTest, Read) {
  int kmsg_fd = open("/dev/kmsg", O_RDWR);
  if (kmsg_fd < 0) {
    fprintf(stderr, "Failed to open /dev/kmsg for writing: %s\n", strerror(errno));
    FAIL();
  }

  // Write a first log.
  dprintf(kmsg_fd, "SyslogRead -- first\n");

  //// Read that first log we wrote.
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
  std::string buf_all;
  buf_all.resize(size);
  int size_read =
      klogctl(3 /* SYSLOG_ACTION_READ_ALL */, buf_all.data(), static_cast<int>(buf_all.size()));
  if (size_read <= 0) {
    fprintf(stderr, "Failed to read: %s\n", strerror(errno));
    FAIL();
  }
  EXPECT_NE(buf_all.find("SyslogRead -- first"), std::string::npos);
  EXPECT_NE(buf_all.find("SyslogRead -- second"), std::string::npos);

  close(kmsg_fd);
}

TEST_F(SyslogTest, ReadProcKmsg) {
  int kmsg_fd = open("/dev/kmsg", O_WRONLY);
  if (kmsg_fd < 0) {
    fprintf(stderr, "Failed to open /dev/kmsg -> %s\n", strerror(errno));
    FAIL();
  }
  dprintf(kmsg_fd, "ReadProcKmsg -- log one\n");

  int proc_kmsg_fd = open("/proc/kmsg", O_RDONLY);
  if (proc_kmsg_fd < 0) {
    fprintf(stderr, "Failed to open /proc/kmsg -> %s\n", strerror(errno));
    FAIL();
  }

  // Read that first log we wrote.
  char buf[4096];
  do {
    size_t size_read = read(proc_kmsg_fd, buf, sizeof(buf));
    ASSERT_GT(size_read, 0ul);
  } while (strstr(buf, "ReadProcKmsg -- log one") == nullptr);

  // Write a second log.
  dprintf(kmsg_fd, "ReadProcKmsg -- log two\n");
  close(kmsg_fd);

  // Check that the first log we read isn't present anymore.
  std::fill_n(buf, 4096, 0);
  do {
    size_t size_read = read(proc_kmsg_fd, buf, sizeof(buf));
    ASSERT_GT(size_read, 0ul);
    EXPECT_EQ(strstr(buf, "ReadProcKmsg -- log one"), nullptr);

  } while (strstr(buf, "ReadProcKmsg -- log two") == nullptr);

  close(proc_kmsg_fd);
}

TEST_F(SyslogTest, NonBlockingRead) {
  int fd = open("/dev/kmsg", O_RDONLY | O_NONBLOCK);
  char buf[4096];
  ssize_t size_read = 0;
  while (size_read != -1) {
    size_read = read(fd, buf, sizeof(buf));
  }
  EXPECT_EQ(errno, EAGAIN);
  close(fd);
}

TEST_F(SyslogTest, ProcKmsgPoll) {
  int kmsg_fd = open("/dev/kmsg", O_WRONLY);
  if (kmsg_fd < 0) {
    fprintf(stderr, "Failed to open /dev/kmsg -> %s\n", strerror(errno));
    FAIL();
  }
  dprintf(kmsg_fd, "ProcKmsgPoll -- log one\n");

  int proc_kmsg_fd = open("/proc/kmsg", O_RDONLY);

  // Drain the logs.
  char buf[4096];
  do {
    size_t size_read = read(proc_kmsg_fd, buf, sizeof(buf));
    ASSERT_GT(size_read, 0ul);
  } while (strstr(buf, "ProcKmsgPoll -- log one") == nullptr);

  struct pollfd fds[] = {{
      .fd = proc_kmsg_fd,
      .events = POLLIN,
      .revents = 42,
  }};

  // With no timeout, this returns immediately.
  EXPECT_EQ(0, poll(fds, 1, 0));

  // Ensure syslog returns that the unread size is 0.
  EXPECT_EQ(0, klogctl(9 /* SYSLOG_ACTION_SIZE_UNREAD */, NULL, 0));

  // Write a log.
  dprintf(kmsg_fd, "ProcKmsgPoll -- log two\n");

  // Wait for the log to be ready to read.
  EXPECT_EQ(1, poll(fds, 1, -1));
  EXPECT_EQ(POLLIN, fds[0].revents);

  // Syslog isn't empty anymore.
  EXPECT_GT(klogctl(9 /* SYSLOG_ACTION_SIZE_UNREAD */, NULL, 0), 0);

  close(kmsg_fd);
  close(proc_kmsg_fd);
}

TEST_F(SyslogTest, DevKmsgSeekSet) {
  int fd = open("/dev/kmsg", O_RDWR);
  if (fd < 0) {
    fprintf(stderr, "Failed to open /dev/kmsg for writing: %s\n", strerror(errno));
    FAIL();
  }
  dprintf(fd, "DevKmsgSeekSet: hello\n");

  // Advance until we have read the log written above.
  char buf[4096];
  do {
    size_t size_read = read(fd, buf, sizeof(buf));
    ASSERT_GT(size_read, 0ul);
  } while (strstr(buf, "DevKmsgSeekSet: hello") == nullptr);

  // Seek to the beginning of the log.
  lseek(fd, 0, SEEK_SET);

  // We see the previous log again. If we had not done SEEK_SET,0. This would hang until some
  // unseen log arrives.
  std::fill_n(buf, 4096, 0);
  do {
    size_t size_read = read(fd, buf, sizeof(buf));
    ASSERT_GT(size_read, 0ul);
  } while (strstr(buf, "DevKmsgSeekSet: hello") == nullptr);

  close(fd);
}

TEST_F(SyslogTest, DevKmsgSeekEnd) {
  int fd = open("/dev/kmsg", O_RDWR);
  if (fd < 0) {
    fprintf(stderr, "Failed to open /dev/kmsg for writing: %s\n", strerror(errno));
    FAIL();
  }
  dprintf(fd, "DevKmsgSeekEnd: hello\n");

  // Ensure the log has been written.
  char buf[4096];
  do {
    size_t size_read = read(fd, buf, sizeof(buf));
    ASSERT_GT(size_read, 0ul);
  } while (strstr(buf, "DevKmsgSeekEnd: hello") == nullptr);
  close(fd);

  // Open a new file, and seek to the end of the log.
  fd = open("/dev/kmsg", O_RDWR | O_NONBLOCK);
  if (fd < 0) {
    fprintf(stderr, "Failed to open /dev/kmsg for writing: %s\n", strerror(errno));
    FAIL();
  }

  lseek(fd, 0, SEEK_END);

  dprintf(fd, "DevKmsgSeekEnd: bye\n");

  // We should see the second log but never the first one.
  std::fill_n(buf, 4096, 0);
  do {
    size_t size_read = read(fd, buf, sizeof(buf));
    ASSERT_GT(size_read, 0ul);
    EXPECT_EQ(strstr(buf, "DevKmsgSeekEnd: hello"), nullptr);
  } while (strstr(buf, "DevKmsgSeekEnd: bye") == nullptr);

  close(fd);
}
