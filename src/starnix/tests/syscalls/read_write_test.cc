// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <sys/mman.h>
#include <sys/uio.h>
#include <unistd.h>

#include <gtest/gtest.h>

#include "src/starnix/tests/syscalls/test_helper.h"

TEST(ReadWriteTest, preadv_pwritev) {
  test_helper::ScopedTempFD temp_file;
  ASSERT_TRUE(temp_file);

  // Add some data to the file and seek back to the beginning so we know the offset took effect.
  constexpr int kOffset = 7;
  const char kOffsetBuffer[kOffset + 1] = "zzzzzzz";
  EXPECT_EQ(kOffset, HANDLE_EINTR(write(temp_file.fd(), kOffsetBuffer, kOffset)));

  constexpr size_t kWriteBufferSize = 32;
  char kWriteBuffer[kWriteBufferSize + 1] = "aaaaaaaabbbbbbbbccccccccdddddddd";

  struct iovec iov[] = {
      {
          .iov_base = &kWriteBuffer[8],  // Pick 8 b's.
          .iov_len = 8,
      },
      {
          .iov_base = &kWriteBuffer[0],  // a's followed by b's
          .iov_len = 16,
      },
      {
          .iov_base = &kWriteBuffer[16],  // c's followed by d's.
          .iov_len = 16,
      },
  };

  // Technically this can do partial writes but our implementation doesn't support this.
  constexpr ssize_t kExpectedSize = 40;
  ssize_t result = HANDLE_EINTR(pwritev(temp_file.fd(), iov, std::size(iov), kOffset));
  EXPECT_EQ(kExpectedSize, result);

  lseek(temp_file.fd(), 0, SEEK_SET);

  // Generate read the vector using the same offsets as above, but pointing into the read buffer.
  char read_buffer[kWriteBufferSize + 1] = {0};
  iov[0].iov_base = &read_buffer[8];
  iov[1].iov_base = &read_buffer[0];
  iov[2].iov_base = &read_buffer[16];
  result = HANDLE_EINTR(preadv(temp_file.fd(), iov, std::size(iov), kOffset));
  EXPECT_EQ(kExpectedSize, result);

  read_buffer[kWriteBufferSize] = 0;  // Null terminate input for comparison.
  EXPECT_STREQ(read_buffer, kWriteBuffer);
}

// TODO(fxbug.dev/117677) implement partial read/write support (we'll also need tests for read(),
//                        writev(), and readv()).
TEST(ReadWriteTest, DISABLED_PartialWrite) {
  test_helper::ScopedTempFD temp_file;
  ASSERT_TRUE(temp_file);

  // Allocate 2 pages and remove permission from the second.
  constexpr size_t kPageSize = 4096;
  const size_t size = 2 * kPageSize;
  void* addr = mmap(0, size, PROT_READ, MAP_ANONYMOUS | MAP_PRIVATE, 0, 0);
  ASSERT_TRUE(addr);

  void* bad_addr = reinterpret_cast<char*>(addr) + kPageSize;
  ASSERT_EQ(0, mprotect(bad_addr, kPageSize, PROT_NONE));

  // Complete bad write.
  errno = 0;
  ssize_t sresult = write(temp_file.fd(), bad_addr, 2);
  EXPECT_EQ(-1, sresult);
  EXPECT_EQ(EFAULT, errno);

  // Partial write, should write the first page and stop on the (invalid) second one.
  errno = 0;
  sresult = write(temp_file.fd(), addr, size);
  EXPECT_EQ(ssize_t(kPageSize), sresult);
  EXPECT_EQ(0, errno);

  // The seek offset should reflect the last partial write.
  off_t off = lseek(temp_file.fd(), 0, SEEK_CUR);
  EXPECT_EQ(off_t(kPageSize), off);

  munmap(addr, size);
}
