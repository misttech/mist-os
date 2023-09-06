// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <sys/mman.h>

#include <gtest/gtest.h>
#include <linux/memfd.h>

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
    fd_ = static_cast<int>(syscall(__NR_memfd_create, "test_memfd", MFD_ALLOW_SEALING));
  }

  void TearDown() override {
    close(fd_);
    if (addr_ != MAP_FAILED) {
      ASSERT_EQ(munmap(addr_, kPageSize), 0);
    }
  }

 protected:
  int fd_ = -1;
  void* addr_ = MAP_FAILED;
};

// These tests currently test only `F_SEAL_FUTURE_WRITE`. `F_SEAL_WRITE` is covered
// by GVisor tests.
TEST_F(MemfdTest, SealFutureWriteThenMmap) {
  ASSERT_EQ(fcntl(fd_, F_ADD_SEALS, F_SEAL_FUTURE_WRITE), 0);

  // Readable mapping succeeds even when created from writable FD.
  addr_ = mmap(0, kPageSize, PROT_READ, MAP_SHARED, fd_, 0);
  ASSERT_NE(addr_, MAP_FAILED);

  // `mprotect()` should fail since the file was sealed for write.
  ASSERT_EQ(mprotect(addr_, kPageSize, PROT_READ | PROT_WRITE), -1);
}

TEST_F(MemfdTest, MmapThenSealFutureWrite) {
  addr_ = mmap(0, kPageSize, PROT_READ, MAP_SHARED, fd_, 0);
  ASSERT_NE(addr_, MAP_FAILED);

  ASSERT_EQ(fcntl(fd_, F_ADD_SEALS, F_SEAL_FUTURE_WRITE), 0);

  // `mprotect()` still succeed since the mapping was created before the seal.
  ASSERT_EQ(mprotect(addr_, kPageSize, PROT_READ | PROT_WRITE), 0);
}

// These tests currently test `F_SEAL_FUTURE_WRITE`. `F_SEAL_WRITE` is covered
// by GVisor tests.
TEST_F(MemfdTest, MapWritableThenSealFutureWrite) {
  addr_ = mmap(0, kPageSize, PROT_READ | PROT_WRITE, MAP_SHARED, fd_, 0);
  ASSERT_NE(addr_, MAP_FAILED);

  // `F_SEAL_FUTURE_WRITE` should still succeed when there are writable mappings.
  ASSERT_EQ(fcntl(fd_, F_ADD_SEALS, F_SEAL_FUTURE_WRITE), 0);
}

}  // namespace
