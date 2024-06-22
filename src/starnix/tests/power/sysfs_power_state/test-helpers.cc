// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "test-helpers.h"

#include <fcntl.h>
#include <unistd.h>

#include <fstream>
#include <string>
#include <thread>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "src/lib/files/file.h"

namespace {
constexpr char kExpectedStr[] = "fake-suspend device attempted suspend";

class fd_buf : public std::streambuf {
 public:
  explicit fd_buf(fbl::unique_fd fd) : std::streambuf(), fd_(std::move(fd)) {}

  fd_buf(fd_buf&& o) {
    std::swap(fd_, o.fd_);
    std::swap(buf_, o.buf_);
    std::swap(cap_, o.cap_);
    std::swap(len_, o.len_);
  }

  ~fd_buf() override {}

  std::streambuf* setbuf(char* s, std::streamsize n) override {
    buf_ = s;
    cap_ = n;
    len_ = 0;

    setg(buf_, buf_, buf_ + len_);
    return this;
  }

  int underflow() override {
    char* gptr_v = gptr();
    if (gptr_v == egptr()) {
      ssize_t ret = read(fd_.get(), buf_, cap_);
      if (ret < 0) {
        ret = 0;
      }

      len_ = ret;
      setg(buf_, buf_, buf_ + len_);

      if (len_ == 0) {
        return std::char_traits<char>::eof();
      }

      gptr_v = buf_;
    }

    return *gptr_v;
  }

 private:
  fbl::unique_fd fd_;

  char* buf_ = nullptr;
  size_t cap_ = 0;
  size_t len_ = 0;
};

std::thread StartThreadToWatchFakeSuspendAttempt() {
  // Open the file before we start the thread so we can guarantee that the expected
  // log from fake-suspend is not yet emitted.
  fbl::unique_fd kmsg(open("/dev/kmsg", O_RDONLY));
  // We can also do this with |fd_buf::pubseekoff| but that requires us to
  // implement |fdbuf::seekoff|.
  EXPECT_EQ(lseek(kmsg.get(), 0, SEEK_END), 0);

  return std::thread([kmsg = std::move(kmsg)]() mutable {
    fd_buf syslogs_fd_buf(std::move(kmsg));
    std::istream syslogs(&syslogs_fd_buf);
    std::string log;

    char buf[4096];
    syslogs.rdbuf()->pubsetbuf(buf, sizeof(buf));
    while (std::getline(syslogs, log)) {
      if (log.find(kExpectedStr) != std::string::npos) {
        return;
      }
    }

    FAIL() << "Did not observe expected string '" << kExpectedStr << "'";
  });
}
}  // namespace

void DoTest(const std::string& file_to_increment, const std::string& file_to_not_change,
            bool expect_success) {
  const std::string filepath_to_increment = "/sys/power/suspend_stats/" + file_to_increment;
  const std::string filepath_to_not_change = "/sys/power/suspend_stats/" + file_to_not_change;
  uint8_t writes = 0;

  auto check_stat = [&](const std::string& filepath, const std::string& expected) {
    std::string read;
    ASSERT_TRUE(files::ReadFileToString(filepath, &read));
    EXPECT_EQ(read, expected);
  };

  auto check_stats = [&]() {
    ASSERT_NO_FATAL_FAILURE(check_stat(filepath_to_not_change, "0\n"));
    ASSERT_NO_FATAL_FAILURE(check_stat(filepath_to_increment, std::to_string(writes) + "\n"));
  };

  auto check_write = [&](const std::string& state) {
    std::thread thrd = StartThreadToWatchFakeSuspendAttempt();
    ASSERT_EQ(files::WriteFile("/sys/power/state", state), expect_success);
    thrd.join();
    ++writes;
    ASSERT_NO_FATAL_FAILURE(check_stats());
  };

  ASSERT_NO_FATAL_FAILURE(check_stats());
  ASSERT_NO_FATAL_FAILURE(check_write("mem"));
  ASSERT_NO_FATAL_FAILURE(check_write("freeze"));
}
