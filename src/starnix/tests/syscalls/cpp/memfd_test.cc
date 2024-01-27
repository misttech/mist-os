// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <sys/poll.h>
#include <sys/uio.h>
#include <unistd.h>

#include <linux/memfd.h>

// Must be included after <linux/*.h> to avoid conflicts between Bionic UAPI and glibc headers.
// TODO(b/307959737): Build these tests without glibc.
#include <sys/mman.h>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "fault_test.h"

#if !defined(__NR_memfd_create)
#if defined(__x86_64__)
#define __NR_memfd_create 319
#elif defined(__aarch64__) || defined(__riscv)
#define __NR_memfd_create 279
#endif
#endif  // !defined(__NR_memfd_create)

#if !defined(MFD_ALLOW_SEALING)
#define MFD_ALLOW_SEALING 0x0002U
#endif

#if !defined(F_SEAL_FUTURE_WRITE)
#define F_SEAL_FUTURE_WRITE 0x0010
#endif

#if !defined(F_ADD_SEALS)
#define F_ADD_SEALS 1033
#endif

namespace {

const int kPageSize = 4096;

class MemfdTest : public ::testing::Test {
  void SetUp() override {
    ASSERT_TRUE(fd_ = fbl::unique_fd(
                    static_cast<int>(syscall(__NR_memfd_create, "test_memfd", MFD_ALLOW_SEALING))))
        << strerror(errno);
  }

  void TearDown() override {
    if (addr_ != MAP_FAILED) {
      ASSERT_EQ(munmap(addr_, kPageSize), 0);
    }
    fd_.reset();
  }

 protected:
  fbl::unique_fd fd_;
  void* addr_ = MAP_FAILED;
};

// These tests currently test only `F_SEAL_FUTURE_WRITE`. `F_SEAL_WRITE` is covered
// by GVisor tests.
TEST_F(MemfdTest, SealFutureWriteThenMmap) {
  ASSERT_EQ(fcntl(fd_.get(), F_ADD_SEALS, F_SEAL_FUTURE_WRITE), 0);

  // Readable mapping succeeds even when created from writable FD.
  addr_ = mmap(0, kPageSize, PROT_READ, MAP_SHARED, fd_.get(), 0);
  ASSERT_NE(addr_, MAP_FAILED);

  // `mprotect()` should fail since the file was sealed for write.
  ASSERT_EQ(mprotect(addr_, kPageSize, PROT_READ | PROT_WRITE), -1);
}

TEST_F(MemfdTest, MmapThenSealFutureWrite) {
  addr_ = mmap(0, kPageSize, PROT_READ, MAP_SHARED, fd_.get(), 0);
  ASSERT_NE(addr_, MAP_FAILED);

  ASSERT_EQ(fcntl(fd_.get(), F_ADD_SEALS, F_SEAL_FUTURE_WRITE), 0);

  // `mprotect()` still succeed since the mapping was created before the seal.
  ASSERT_EQ(mprotect(addr_, kPageSize, PROT_READ | PROT_WRITE), 0);
}

// These tests currently test `F_SEAL_FUTURE_WRITE`. `F_SEAL_WRITE` is covered
// by GVisor tests.
TEST_F(MemfdTest, MapWritableThenSealFutureWrite) {
  addr_ = mmap(0, kPageSize, PROT_READ | PROT_WRITE, MAP_SHARED, fd_.get(), 0);
  ASSERT_NE(addr_, MAP_FAILED);

  // `F_SEAL_FUTURE_WRITE` should still succeed when there are writable mappings.
  ASSERT_EQ(fcntl(fd_.get(), F_ADD_SEALS, F_SEAL_FUTURE_WRITE), 0);
}

class MemfdFaultTest : public FaultTest<MemfdTest> {
 protected:
  void SetFdNonBlocking() {
    int flags = fcntl(fd_.get(), F_GETFL, 0);
    ASSERT_GE(flags, 0) << strerror(errno);
    ASSERT_EQ(fcntl(fd_.get(), F_SETFL, flags | O_NONBLOCK), 0) << strerror(errno);
  }
};

TEST_F(MemfdFaultTest, Write) {
  ASSERT_EQ(write(fd_.get(), faulting_ptr_, kFaultingSize_), -1);
  EXPECT_EQ(errno, EFAULT);
}

TEST_F(MemfdFaultTest, Read) {
  // First send a valid message that we can read.
  constexpr char kWriteBuf[] = "Hello world";
  ASSERT_EQ(write(fd_.get(), &kWriteBuf, sizeof(kWriteBuf)),
            static_cast<ssize_t>(sizeof(kWriteBuf)));
  ASSERT_EQ(lseek(fd_.get(), 0, SEEK_SET), 0) << strerror(errno);

  pollfd p = {
      .fd = fd_.get(),
      .events = POLLIN,
  };
  ASSERT_EQ(poll(&p, 1, -1), 1);
  ASSERT_EQ(p.revents, POLLIN);

  static_assert(kFaultingSize_ >= sizeof(kWriteBuf));
  ASSERT_EQ(read(fd_.get(), faulting_ptr_, sizeof(kWriteBuf)), -1);
  EXPECT_EQ(errno, EFAULT);

  // Previous read failed so we should be able to read all the written bytes
  // here.
  char read_buf[sizeof(kWriteBuf)] = {};
  ASSERT_EQ(read(fd_.get(), read_buf, sizeof(read_buf)), static_cast<ssize_t>(sizeof(kWriteBuf)));
  EXPECT_STREQ(read_buf, kWriteBuf);
}

TEST_F(MemfdFaultTest, ReadV) {
  // First send a valid message that we can read.
  constexpr char kWriteBuf[] = "Hello world";
  ASSERT_EQ(write(fd_.get(), &kWriteBuf, sizeof(kWriteBuf)),
            static_cast<ssize_t>(sizeof(kWriteBuf)));
  ASSERT_EQ(lseek(fd_.get(), 0, SEEK_SET), 0) << strerror(errno);

  pollfd p = {
      .fd = fd_.get(),
      .events = POLLIN,
  };
  ASSERT_EQ(poll(&p, 1, -1), 1);
  ASSERT_EQ(p.revents, POLLIN);

  char base0[1] = {};
  char base2[sizeof(kWriteBuf) - sizeof(base0)] = {};
  iovec iov[] = {
      {
          .iov_base = base0,
          .iov_len = sizeof(base0),
      },
      {
          .iov_base = faulting_ptr_,
          .iov_len = sizeof(kFaultingSize_),
      },
      {
          .iov_base = base2,
          .iov_len = sizeof(base2),
      },
  };

  // Read once with iov holding the invalid pointer. We should perform
  // a partial read.
  ASSERT_EQ(readv(fd_.get(), iov, std::size(iov)), 1);
  ASSERT_EQ(base0[0], kWriteBuf[0]);

  // Read the rest.
  iov[0] = iovec{};
  iov[1] = iovec{};
  ASSERT_EQ(readv(fd_.get(), iov, std::size(iov)), static_cast<ssize_t>(sizeof(base2)));
  EXPECT_STREQ(base2, &kWriteBuf[1]);
}

TEST_F(MemfdFaultTest, WriteV) {
  char write_buf[] = "Hello world";
  constexpr size_t kBase0Size = 1;
  iovec iov[] = {
      {
          .iov_base = write_buf,
          .iov_len = kBase0Size,
      },
      {
          .iov_base = faulting_ptr_,
          .iov_len = sizeof(kFaultingSize_),
      },
      {
          .iov_base = reinterpret_cast<char*>(write_buf) + kBase0Size,
          .iov_len = sizeof(write_buf) - kBase0Size,
      },
  };

  // Write with iov holding the invalid pointer.
  ASSERT_EQ(writev(fd_.get(), iov, std::size(iov)), -1);
  EXPECT_EQ(errno, EFAULT);
  ASSERT_EQ(lseek(fd_.get(), 0, SEEK_SET), 0) << strerror(errno);

  // The memfd should have no size since the above write failed.
  ASSERT_NO_FATAL_FAILURE(SetFdNonBlocking());
  char recv_buf[sizeof(write_buf)];
  EXPECT_EQ(read(fd_.get(), recv_buf, sizeof(recv_buf)), 0);
}

}  // namespace
