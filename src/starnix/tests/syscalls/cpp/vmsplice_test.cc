// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <lib/fit/defer.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <unistd.h>

#include <cstdlib>
#include <string>

#include <fbl/unique_fd.h>
#include <gmock/gmock.h>
#include <gtest/gtest.h>

namespace {

constexpr size_t kNumPipeEndsCount = 2;

void MakePipe(fbl::unique_fd (&fds)[kNumPipeEndsCount]) {
  int raw_fds[kNumPipeEndsCount] = {};
  ASSERT_EQ(pipe(raw_fds), 0) << strerror(errno);
  for (size_t i = 0; i < kNumPipeEndsCount; ++i) {
    fds[i] = fbl::unique_fd(raw_fds[i]);
    ASSERT_TRUE(fds[i]);
  }
}

TEST(VmspliceTest, VmspliceReadClosedPipe) {
  fbl::unique_fd fds[kNumPipeEndsCount];
  ASSERT_NO_FATAL_FAILURE(MakePipe(fds));
  ASSERT_EQ(fds[1].reset(), 0) << strerror(errno);
  char buffer[4096] = {};
  iovec iov = {
      .iov_base = buffer,
      .iov_len = sizeof(buffer),
  };
  ASSERT_EQ(vmsplice(fds[0].get(), &iov, 1, SPLICE_F_NONBLOCK), 0) << strerror(errno);
  ASSERT_EQ(vmsplice(fds[0].get(), &iov, 1, 0), 0) << strerror(errno);
}

TEST(VmspliceTest, VmspliceWriteClosedPipe) {
  constexpr int kRetCode = 42;
  EXPECT_EXIT(([]() {
                signal(SIGPIPE, SIG_IGN);

                fbl::unique_fd fds[kNumPipeEndsCount];
                ASSERT_NO_FATAL_FAILURE(MakePipe(fds));
                ASSERT_EQ(fds[0].reset(), 0) << strerror(errno);
                char buffer[4096] = {};
                iovec iov = {
                    .iov_base = buffer,
                    .iov_len = sizeof(buffer),
                };
                ASSERT_EQ(vmsplice(fds[1].get(), &iov, 1, SPLICE_F_NONBLOCK), -1);
                ASSERT_EQ(errno, EPIPE);
                ASSERT_EQ(vmsplice(fds[1].get(), &iov, 1, 0), -1);
                ASSERT_EQ(errno, EPIPE);
                exit(kRetCode);
              })(),
              testing::ExitedWithCode(kRetCode), "");
}

TEST(VmspliceTest, ModifyBufferInPipe) {
  constexpr char kInitialByte = 'A';
  constexpr char kModifiedByte = 'B';

  fbl::unique_fd fds[kNumPipeEndsCount];
  ASSERT_NO_FATAL_FAILURE(MakePipe(fds));
  // Volatile to let the compiler know not to optimize out the final write we
  // perform to `write_buffer` which is never directly read here.
  volatile char write_buffer[10];
  std::fill_n(&write_buffer[0], std::size(write_buffer), kInitialByte);
  {
    iovec iov = {
        .iov_base = const_cast<char*>(write_buffer),
        .iov_len = sizeof(write_buffer),
    };
    ASSERT_EQ(vmsplice(fds[1].get(), &iov, 1, 0), static_cast<ssize_t>(sizeof(write_buffer)))
        << strerror(errno);
  }
  std::fill_n(&write_buffer[0], std::size(write_buffer), kModifiedByte);

  char read_buffer[sizeof(write_buffer)] = {};
  ASSERT_EQ(read(fds[0].get(), read_buffer, sizeof(read_buffer)),
            static_cast<ssize_t>(sizeof(read_buffer)))
      << strerror(errno);
  EXPECT_THAT(read_buffer, testing::Each(kModifiedByte));
}

TEST(VmspliceTest, UnmapBufferInPipe) {
  constexpr size_t kMmapSize = 4096;
  constexpr char kInitialByte = 'A';
  constexpr char kModifiedByte = 'B';

  fbl::unique_fd fds[kNumPipeEndsCount];
  ASSERT_NO_FATAL_FAILURE(MakePipe(fds));
  {
    // Volatile to let the compiler know not to optimize out the final write we
    // perform to `write_buffer` which is never directly read here.
    volatile char* write_buffer = reinterpret_cast<volatile char*>(
        mmap(NULL, kMmapSize, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0));
    ASSERT_NE(write_buffer, MAP_FAILED) << strerror(errno);
    auto unmap_write_buffer = fit::defer([&]() {
      EXPECT_EQ(munmap(const_cast<char*>(write_buffer), kMmapSize), 0) << strerror(errno);
    });
    std::fill_n(write_buffer, kMmapSize, kInitialByte);
    {
      iovec iov = {
          .iov_base = const_cast<char*>(write_buffer),
          .iov_len = kMmapSize,
      };
      ASSERT_EQ(vmsplice(fds[1].get(), &iov, 1, 0), static_cast<ssize_t>(kMmapSize))
          << strerror(errno);
    }
    std::fill_n(&write_buffer[0], kMmapSize, kModifiedByte);
  }

  char read_buffer[kMmapSize] = {};
  ASSERT_EQ(read(fds[0].get(), read_buffer, sizeof(read_buffer)),
            static_cast<ssize_t>(sizeof(read_buffer)))
      << strerror(errno);
  EXPECT_THAT(read_buffer, testing::Each(kModifiedByte));
}

TEST(VmspliceTest, UnmapBufferInPipeThenMapInPlace) {
  constexpr size_t kMmapSize = 4096;
  constexpr char kInitialByte = 'A';
  constexpr char kModifiedByte = 'B';

  fbl::unique_fd fds[kNumPipeEndsCount];
  ASSERT_NO_FATAL_FAILURE(MakePipe(fds));
  // Volatile to let the compiler know not to optimize out the final write we
  // perform to `write_buffer` which is never directly read here.
  volatile char* write_buffer = reinterpret_cast<volatile char*>(
      mmap(NULL, kMmapSize, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0));
  {
    ASSERT_NE(write_buffer, MAP_FAILED) << strerror(errno);
    auto unmap_write_buffer = fit::defer([&]() {
      EXPECT_EQ(munmap(const_cast<char*>(write_buffer), kMmapSize), 0) << strerror(errno);
    });
    std::fill_n(write_buffer, kMmapSize, kInitialByte);
    {
      iovec iov = {
          .iov_base = const_cast<char*>(write_buffer),
          .iov_len = kMmapSize,
      };
      ASSERT_EQ(vmsplice(fds[1].get(), &iov, 1, 0), static_cast<ssize_t>(kMmapSize))
          << strerror(errno);
    }
    std::fill_n(&write_buffer[0], kMmapSize, kModifiedByte);
  }

  {
    // Volatile to let the compiler know not to optimize out the final write we
    // perform to `write_buffer_new` which is never read here.
    volatile char* write_buffer_new = reinterpret_cast<volatile char*>(
        mmap(const_cast<char*>(write_buffer), kMmapSize, PROT_READ | PROT_WRITE,
             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0));
    ASSERT_NE(write_buffer_new, MAP_FAILED) << strerror(errno);
    ASSERT_EQ(write_buffer, write_buffer_new);
    auto unmap_write_buffer = fit::defer([&]() {
      EXPECT_EQ(munmap(const_cast<char*>(write_buffer_new), kMmapSize), 0) << strerror(errno);
    });
    std::fill_n(&write_buffer[0], kMmapSize, kInitialByte);
  }

  char read_buffer[kMmapSize] = {};
  ASSERT_EQ(read(fds[0].get(), read_buffer, sizeof(read_buffer)),
            static_cast<ssize_t>(sizeof(read_buffer)))
      << strerror(errno);
  EXPECT_THAT(read_buffer, testing::Each(kModifiedByte));
}

}  // namespace
