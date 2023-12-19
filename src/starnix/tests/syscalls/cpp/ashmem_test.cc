// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

#include <gmock/gmock.h>
#include <gtest/gtest.h>
#include <linux/ashmem.h>

#include "src/starnix/tests/syscalls/cpp/syscall_matchers.h"
#include "src/starnix/tests/syscalls/cpp/test_helper.h"

namespace {

class AshmemTest : public ::testing::Test {
  void SetUp() override {
    // /dev/ashmem must exist
    if (access("/dev/ashmem", F_OK) != 0) {
      GTEST_SKIP() << "/dev/ashmem is not present.";
    }
  }

 protected:
  static test_helper::ScopedFD Open() {
    test_helper::ScopedFD ashmem_fd(open("/dev/ashmem", O_RDWR));
    return ashmem_fd;
  }
};

TEST_F(AshmemTest, Open) {
  auto ashmem_fd = Open();
  ASSERT_TRUE(ashmem_fd.is_valid());
}

TEST_F(AshmemTest, SetSize) {
  auto ashmem = Open();
  ASSERT_TRUE(ashmem.is_valid());
  ASSERT_THAT(ioctl(ashmem.get(), ASHMEM_GET_SIZE), SyscallSucceedsWithValue(0));
  ASSERT_THAT(ioctl(ashmem.get(), ASHMEM_SET_SIZE, 0x1000), SyscallSucceeds());
}

TEST_F(AshmemTest, NoAccessBeforeSetSize) {
  auto ashmem = Open();
  ASSERT_TRUE(ashmem.is_valid());

  errno = 0;
  void *map = mmap(nullptr, 0x1000, PROT_READ, MAP_PRIVATE, ashmem.get(), 0);
  ASSERT_TRUE(map == MAP_FAILED && errno == EINVAL) << "mmap failed with " << strerror(errno);

  char buf[10] = "";
  ASSERT_THAT(pread(ashmem.get(), buf, sizeof(buf), 0), SyscallFailsWithErrno(EINVAL));
  ASSERT_THAT(pwrite(ashmem.get(), buf, sizeof(buf), 0), SyscallFailsWithErrno(EINVAL));
}

TEST_F(AshmemTest, NoSetSizeAfterMap) {
  auto ashmem = Open();
  ASSERT_TRUE(ashmem.is_valid());
  ASSERT_THAT(ioctl(ashmem.get(), ASHMEM_SET_SIZE, 0x2000), SyscallSucceeds());
  ASSERT_THAT(ioctl(ashmem.get(), ASHMEM_SET_SIZE, 0x1000), SyscallSucceeds());

  void *map = mmap(nullptr, 0x1000, PROT_READ, MAP_PRIVATE, ashmem.get(), 0);
  ASSERT_TRUE(map != nullptr) << "mmap failed with " << strerror(errno);
  ASSERT_THAT(munmap(map, 0x1000), SyscallSucceeds());

  ASSERT_THAT(ioctl(ashmem.get(), ASHMEM_SET_SIZE, 0x1000), SyscallFailsWithErrno(EINVAL));
}

}  // namespace
