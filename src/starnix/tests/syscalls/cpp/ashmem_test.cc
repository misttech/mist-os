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

const unsigned PAGE_SIZE = getpagesize();
const unsigned long long LARGE_NUMBER_OF_PAGES = PAGE_SIZE * 1000 * 1000;

class AshmemTest : public ::testing::Test {
  void SetUp() override {
    // /dev/ashmem must exist
    if (access("/dev/ashmem", F_OK) != 0) {
      GTEST_SKIP() << "/dev/ashmem is not present.";
    }

    errno = 0;
  }

 protected:
  static fbl::unique_fd Open() {
    fbl::unique_fd fd(open("/dev/ashmem", O_RDWR));
    EXPECT_TRUE(fd.is_valid());
    return fd;
  }

  static fbl::unique_fd CreateRegion(char name[], size_t size) {
    fbl::unique_fd fd(open("/dev/ashmem", O_RDWR));
    EXPECT_TRUE(fd.is_valid());
    EXPECT_THAT(ioctl(fd.get(), ASHMEM_SET_SIZE, size), SyscallSucceeds());
    if (name != nullptr) {
      EXPECT_THAT(ioctl(fd.get(), ASHMEM_SET_NAME, name), SyscallSucceeds());
    }
    return fd;
  }
};

// Can open "/dev/ashmem"
TEST_F(AshmemTest, Open) {
  auto fd = Open();
  ASSERT_TRUE(fd.is_valid());
  ASSERT_THAT(close(fd.get()), SyscallSucceeds());
}

TEST_F(AshmemTest, DefaultSize) {
  auto fd = Open();
  ASSERT_TRUE(fd.is_valid());
  EXPECT_THAT(ioctl(fd.get(), ASHMEM_GET_SIZE), SyscallSucceedsWithValue(0));
  ASSERT_THAT(ioctl(fd.get(), ASHMEM_SET_SIZE, PAGE_SIZE), SyscallSucceeds());
  EXPECT_THAT(ioctl(fd.get(), ASHMEM_GET_SIZE), SyscallSucceedsWithValue((int)PAGE_SIZE));
  ASSERT_THAT(close(fd.get()), SyscallSucceeds());
}

TEST_F(AshmemTest, DefaultName) {
  char buf[256];
  auto fd = Open();
  ASSERT_TRUE(fd.is_valid());
  ASSERT_THAT(ioctl(fd.get(), ASHMEM_GET_NAME, buf), SyscallSucceeds());
  EXPECT_STREQ("dev/ashmem", buf);
  ASSERT_THAT(ioctl(fd.get(), ASHMEM_SET_NAME, "test"), SyscallSucceeds());
  ASSERT_THAT(ioctl(fd.get(), ASHMEM_GET_NAME, buf), SyscallSucceeds());
  EXPECT_STREQ("test", buf);
  ASSERT_THAT(close(fd.get()), SyscallSucceeds());
}

TEST_F(AshmemTest, DefaultProtections) {
  auto fd = Open();
  ASSERT_TRUE(fd.is_valid());
  EXPECT_THAT(ioctl(fd.get(), ASHMEM_GET_PROT_MASK),
              SyscallSucceedsWithValue(PROT_READ | PROT_WRITE | PROT_EXEC));
  ASSERT_THAT(ioctl(fd.get(), ASHMEM_SET_PROT_MASK, PROT_READ), SyscallSucceeds());
  EXPECT_THAT(ioctl(fd.get(), ASHMEM_GET_PROT_MASK), SyscallSucceedsWithValue(PROT_READ));
  ASSERT_THAT(close(fd.get()), SyscallSucceeds());
}

// There must be a size associated with the region for mmap() to succeed
TEST_F(AshmemTest, NoAccessBeforeSetSize) {
  auto fd = Open();
  ASSERT_TRUE(fd.is_valid());

  // No size, fail
  void *addr = mmap(nullptr, PAGE_SIZE, PROT_READ, MAP_SHARED, fd.get(), 0);
  EXPECT_TRUE(addr == MAP_FAILED && errno == EINVAL);

  // With size, succeed
  ASSERT_THAT(ioctl(fd.get(), ASHMEM_SET_SIZE, PAGE_SIZE), SyscallSucceeds());
  addr = mmap(nullptr, PAGE_SIZE, PROT_READ, MAP_SHARED, fd.get(), 0);
  EXPECT_TRUE(addr != MAP_FAILED && addr != nullptr);

  ASSERT_THAT(close(fd.get()), SyscallSucceeds());
}

// Setting the size to zero does not permit mmap()
TEST_F(AshmemTest, SetSizeZero) {
  auto fd = Open();
  ASSERT_TRUE(fd.is_valid());
  ASSERT_THAT(ioctl(fd.get(), ASHMEM_SET_SIZE, 0), SyscallSucceeds());

  void *addr = mmap(nullptr, PAGE_SIZE, PROT_READ, MAP_SHARED, fd.get(), 0);
  EXPECT_TRUE(addr == MAP_FAILED && errno == EINVAL);

  ASSERT_THAT(close(fd.get()), SyscallSucceeds());
}

// Size can be impossibly large
TEST_F(AshmemTest, SetSizeOverflow) {
  auto fd = Open();
  ASSERT_TRUE(fd.is_valid());
  ASSERT_THAT(ioctl(fd.get(), ASHMEM_SET_SIZE, LARGE_NUMBER_OF_PAGES), SyscallSucceeds());

  void *addr = mmap(nullptr, PAGE_SIZE, PROT_READ, MAP_SHARED, fd.get(), 0);
  EXPECT_TRUE(addr != MAP_FAILED && addr != nullptr);

  ASSERT_THAT(munmap(addr, PAGE_SIZE), SyscallSucceeds());
  ASSERT_THAT(close(fd.get()), SyscallSucceeds());
}

// Size rounds up to page boundary, but this is hidden from us
TEST_F(AshmemTest, SetSizeMisaligned) {
  auto fd = Open();
  ASSERT_TRUE(fd.is_valid());
  EXPECT_THAT(ioctl(fd.get(), ASHMEM_SET_SIZE, 1), SyscallSucceeds());
  EXPECT_THAT(ioctl(fd.get(), ASHMEM_GET_SIZE), SyscallSucceedsWithValue(1));

  EXPECT_THAT(ioctl(fd.get(), ASHMEM_SET_SIZE, PAGE_SIZE + 1), SyscallSucceeds());
  EXPECT_THAT(ioctl(fd.get(), ASHMEM_GET_SIZE), SyscallSucceedsWithValue((int)PAGE_SIZE + 1));

  void *addr = mmap(nullptr, 2 * PAGE_SIZE, PROT_WRITE, MAP_SHARED, fd.get(), 0);
  EXPECT_TRUE(addr != MAP_FAILED && addr != nullptr);
  // This will not segfault because size rounds up to two pages despite reporting PAGE_SIZE + 1
  for (size_t i = 0; i < 2 * PAGE_SIZE; i++) {
    reinterpret_cast<volatile uint8_t *>(addr)[i] = 0;
  }

  ASSERT_THAT(munmap(addr, PAGE_SIZE), SyscallSucceeds());
  ASSERT_THAT(close(fd.get()), SyscallSucceeds());
}

// Once mmap() succeeds on a region, the size is set in stone
TEST_F(AshmemTest, NoChangeSizeAfterMap) {
  auto fd = CreateRegion(nullptr, 2 * PAGE_SIZE);
  ASSERT_THAT(ioctl(fd.get(), ASHMEM_SET_SIZE, PAGE_SIZE), SyscallSucceeds());

  void *addr = mmap(nullptr, PAGE_SIZE, PROT_READ, MAP_SHARED, fd.get(), 0);
  ASSERT_TRUE(addr != MAP_FAILED && addr != nullptr);
  EXPECT_THAT(ioctl(fd.get(), ASHMEM_SET_SIZE, PAGE_SIZE), SyscallFailsWithErrno(EINVAL));
  EXPECT_THAT(ioctl(fd.get(), ASHMEM_SET_SIZE, 3 * PAGE_SIZE), SyscallFailsWithErrno(EINVAL));

  ASSERT_THAT(munmap(addr, PAGE_SIZE), SyscallSucceeds());
  ASSERT_THAT(close(fd.get()), SyscallSucceeds());
}

// Mmap() arguments must not be out of bounds
TEST_F(AshmemTest, MapOutOfBounds) {
  auto fd = CreateRegion(nullptr, 3 * PAGE_SIZE);

  // Fill up data
  void *addr = mmap(nullptr, 3 * PAGE_SIZE, PROT_WRITE, MAP_SHARED, fd.get(), 0);
  for (size_t i = 0; i < 3 * PAGE_SIZE / sizeof(int); i++) {
    ((int *)addr)[i] = (int)i;
  }
  ASSERT_THAT(munmap(addr, 3 * PAGE_SIZE), SyscallSucceeds());

  // Too big
  // Ashmem region:     [    3 pages    ]
  // Attempted to mmap: [       4 pages       ]
  addr = mmap(nullptr, 4 * PAGE_SIZE, PROT_READ, MAP_SHARED, fd.get(), 0);
  EXPECT_TRUE(addr == MAP_FAILED && errno == EINVAL);

  // Completely outside bounds
  // TODO: Linux allows mmap() to succeed if the address is outside the region
  // but the size is within the region, which seems like a bug
  // Ashmem region:     [    3 pages    ]
  // Attempted to mmap:                 [  1 page  ]
  addr = mmap(nullptr, PAGE_SIZE, PROT_READ, MAP_SHARED, fd.get(), 4 * PAGE_SIZE);
  EXPECT_TRUE(addr != MAP_FAILED && addr != nullptr);
  ASSERT_THAT(munmap(addr, PAGE_SIZE), SyscallSucceeds());

  // Partially outside bounds
  // Ashmem region:     [    3 pages    ]
  // Attempted to mmap:            [  2 pages  ]
  addr = mmap(nullptr, 2 * PAGE_SIZE, PROT_READ, MAP_SHARED, fd.get(), 2 * PAGE_SIZE);
  EXPECT_TRUE(addr != MAP_FAILED && addr != nullptr);
  // TODO: find a way to verify that accessing any page beyond the first segfaults
  for (size_t i = 0; i < PAGE_SIZE / sizeof(int); i++) {
    int val = (int)(i + 2 * PAGE_SIZE / sizeof(int));
    ASSERT_EQ(val, ((int *)addr)[i]);
  }

  ASSERT_THAT(munmap(addr, PAGE_SIZE), SyscallSucceeds());
  ASSERT_THAT(close(fd.get()), SyscallSucceeds());
}

// Once mmap() has been called the name is set in stone
TEST_F(AshmemTest, NoChangeNameAfterMap) {
  char name[] = "hello world!";
  char buf[256];
  auto fd = CreateRegion(name, PAGE_SIZE);

  void *addr = mmap(nullptr, PAGE_SIZE, PROT_READ, MAP_SHARED, fd.get(), 0);
  ASSERT_TRUE(addr != MAP_FAILED && addr != nullptr);

  ASSERT_THAT(ioctl(fd.get(), ASHMEM_SET_NAME, "goodbye"), SyscallFailsWithErrno(EINVAL));
  ASSERT_THAT(ioctl(fd.get(), ASHMEM_GET_NAME, buf), SyscallSucceeds());
  EXPECT_STREQ("hello world!", buf);

  ASSERT_THAT(munmap(addr, PAGE_SIZE), SyscallSucceeds());
  ASSERT_THAT(close(fd.get()), SyscallSucceeds());
}

// Once mmap() has been called if there was no name before, there will never be a name
TEST_F(AshmemTest, NoSetNameAfterMap) {
  auto fd = CreateRegion(nullptr, PAGE_SIZE);

  void *addr = mmap(nullptr, PAGE_SIZE, PROT_READ, MAP_SHARED, fd.get(), 0);
  ASSERT_TRUE(addr != MAP_FAILED && addr != nullptr);

  EXPECT_THAT(ioctl(fd.get(), ASHMEM_SET_NAME, "test"), SyscallFailsWithErrno(EINVAL));
  ASSERT_THAT(munmap(addr, PAGE_SIZE), SyscallSucceeds());
  ASSERT_THAT(close(fd.get()), SyscallSucceeds());
}

// A bad pointer passed to set name ioctl will fail
TEST_F(AshmemTest, MalformedName) {
  auto fd = Open();
  ASSERT_TRUE(fd.is_valid());
  EXPECT_THAT(ioctl(fd.get(), ASHMEM_SET_NAME, 0), SyscallFailsWithErrno(EFAULT));
  EXPECT_THAT(ioctl(fd.get(), ASHMEM_SET_NAME, 1234), SyscallFailsWithErrno(EFAULT));
  ASSERT_THAT(close(fd.get()), SyscallSucceeds());
}

// Truncate names with length 256 or greater
TEST_F(AshmemTest, SetNameOverflow) {
  char name[300];
  char buf[300];
  memset(name, 'e', 300);  // Long string of 'e' s
  name[299] = 0;           // Null terminator

  auto fd = Open();
  ASSERT_TRUE(fd.is_valid());
  ASSERT_THAT(ioctl(fd.get(), ASHMEM_SET_NAME, name), SyscallSucceeds());
  ASSERT_THAT(ioctl(fd.get(), ASHMEM_GET_NAME, buf), SyscallSucceeds());
  EXPECT_EQ(255, (int)strlen(buf));  // Truncated string
  ASSERT_THAT(close(fd.get()), SyscallSucceeds());
}

// You are not allowed to increase protections
TEST_F(AshmemTest, NoIncreaseProtections) {
  auto fd = Open();
  ASSERT_TRUE(fd.is_valid());
  EXPECT_THAT(ioctl(fd.get(), ASHMEM_SET_PROT_MASK, PROT_READ), SyscallSucceeds());
  EXPECT_THAT(ioctl(fd.get(), ASHMEM_SET_PROT_MASK, PROT_WRITE), SyscallFailsWithErrno(EINVAL));
  EXPECT_THAT(ioctl(fd.get(), ASHMEM_SET_PROT_MASK, PROT_READ | PROT_WRITE),
              SyscallFailsWithErrno(EINVAL));
  ASSERT_THAT(close(fd.get()), SyscallSucceeds());
}

// You are allowed to decrease protections, but changes are not retroactive
TEST_F(AshmemTest, DecreaseProtections) {
  auto fd = Open();
  ASSERT_TRUE(fd.is_valid());
  ASSERT_THAT(ioctl(fd.get(), ASHMEM_SET_SIZE, PAGE_SIZE), SyscallSucceeds());
  ASSERT_THAT(ioctl(fd.get(), ASHMEM_SET_PROT_MASK, PROT_READ | PROT_WRITE), SyscallSucceeds());

  void *addr_rw = mmap(nullptr, PAGE_SIZE, PROT_READ | PROT_WRITE, MAP_SHARED, fd.get(), 0);
  ASSERT_TRUE(addr_rw != MAP_FAILED && addr_rw != nullptr);

  // Decrease protections
  ASSERT_THAT(ioctl(fd.get(), ASHMEM_SET_PROT_MASK, PROT_READ), SyscallSucceeds());
  void *addr_r = mmap(nullptr, PAGE_SIZE, PROT_READ, MAP_SHARED, fd.get(), 0);
  ASSERT_TRUE(addr_r != MAP_FAILED && addr_r != nullptr);

  // Not retroactive; we can still write through addr_rw
  *(int *)addr_rw = 123;  // This would segfault if changes were retroactive
  EXPECT_EQ(123, *(int *)addr_rw);

  ASSERT_THAT(ioctl(fd.get(), ASHMEM_GET_PROT_MASK), SyscallSucceedsWithValue(PROT_READ));

  ASSERT_THAT(munmap(addr_rw, PAGE_SIZE), SyscallSucceeds());
  ASSERT_THAT(munmap(addr_r, PAGE_SIZE), SyscallSucceeds());
  ASSERT_THAT(close(fd.get()), SyscallSucceeds());
}

// Mapping with protections not allowed by the region will fail
TEST_F(AshmemTest, MapProtectionsAgree) {
  auto fd = CreateRegion(nullptr, PAGE_SIZE);

  // PROT_READ | PROT_WRITE
  ASSERT_THAT(ioctl(fd.get(), ASHMEM_SET_PROT_MASK, PROT_READ | PROT_WRITE), SyscallSucceeds());
  void *addr = mmap(nullptr, PAGE_SIZE, PROT_READ | PROT_WRITE, MAP_SHARED, fd.get(), 0);
  EXPECT_TRUE(addr != MAP_FAILED && addr != nullptr);
  ASSERT_THAT(munmap(addr, PAGE_SIZE), SyscallSucceeds());

  addr = mmap(nullptr, PAGE_SIZE, PROT_READ, MAP_SHARED, fd.get(), 0);
  EXPECT_TRUE(addr != MAP_FAILED && addr != nullptr);
  ASSERT_THAT(munmap(addr, PAGE_SIZE), SyscallSucceeds());

  addr = mmap(nullptr, PAGE_SIZE, PROT_WRITE, MAP_SHARED, fd.get(), 0);
  EXPECT_TRUE(addr != MAP_FAILED && addr != nullptr);
  ASSERT_THAT(munmap(addr, PAGE_SIZE), SyscallSucceeds());

  // PROT_READ
  ASSERT_THAT(ioctl(fd.get(), ASHMEM_SET_PROT_MASK, PROT_READ), SyscallSucceeds());

  addr = mmap(nullptr, PAGE_SIZE, PROT_WRITE, MAP_SHARED, fd.get(), 0);
  EXPECT_TRUE(addr == MAP_FAILED && errno == EINVAL);
  ASSERT_THAT(close(fd.get()), SyscallSucceeds());
}

// The set protections ioctl will fail on malformed input
TEST_F(AshmemTest, MalformedProtections) {
  auto fd = Open();
  ASSERT_TRUE(fd.is_valid());
  EXPECT_THAT(ioctl(fd.get(), ASHMEM_SET_PROT_MASK, 8), SyscallFailsWithErrno(EINVAL));
  EXPECT_THAT(ioctl(fd.get(), ASHMEM_SET_PROT_MASK, 27), SyscallFailsWithErrno(EINVAL));
  EXPECT_THAT(ioctl(fd.get(), ASHMEM_SET_PROT_MASK, 1002), SyscallFailsWithErrno(EINVAL));
  EXPECT_THAT(ioctl(fd.get(), ASHMEM_SET_PROT_MASK, "hello"), SyscallFailsWithErrno(EINVAL));
  EXPECT_THAT(ioctl(fd.get(), ASHMEM_SET_PROT_MASK, -1), SyscallFailsWithErrno(EINVAL));
  ASSERT_THAT(close(fd.get()), SyscallSucceeds());
}

TEST_F(AshmemTest, MapPrivate) {
  auto fd = CreateRegion(nullptr, PAGE_SIZE);

  void *addr = mmap(nullptr, PAGE_SIZE, PROT_READ | PROT_WRITE, MAP_PRIVATE, fd.get(), 0);
  EXPECT_TRUE(addr != MAP_FAILED && addr != nullptr);

  ASSERT_THAT(munmap(addr, PAGE_SIZE), SyscallSucceeds());
  ASSERT_THAT(close(fd.get()), SyscallSucceeds());
}

// No pinning permitted unless previously mapped
TEST_F(AshmemTest, NoPinBeforeMap) {
  ashmem_pin pin = {.offset = 0, .len = (uint32_t)PAGE_SIZE};

  auto fd = CreateRegion(nullptr, PAGE_SIZE);
  EXPECT_THAT(ioctl(fd.get(), ASHMEM_PIN, &pin), SyscallFailsWithErrno(EINVAL));
  EXPECT_THAT(ioctl(fd.get(), ASHMEM_UNPIN, &pin), SyscallFailsWithErrno(EINVAL));
  EXPECT_THAT(ioctl(fd.get(), ASHMEM_GET_PIN_STATUS, &pin), SyscallFailsWithErrno(EINVAL));

  void *addr = mmap(nullptr, PAGE_SIZE, PROT_READ | PROT_WRITE, MAP_SHARED, fd.get(), 0);
  ASSERT_TRUE(addr != MAP_FAILED && addr != nullptr);

  EXPECT_THAT(ioctl(fd.get(), ASHMEM_PIN, &pin), SyscallSucceeds());
  EXPECT_THAT(ioctl(fd.get(), ASHMEM_UNPIN, &pin), SyscallSucceeds());

  ASSERT_THAT(munmap(addr, PAGE_SIZE), SyscallSucceeds());
  ASSERT_THAT(close(fd.get()), SyscallSucceeds());
}

// Ashmem regions are pinned by default
TEST_F(AshmemTest, DefaultPin) {
  ashmem_pin pin = {.offset = 0, .len = (uint32_t)PAGE_SIZE};
  auto fd = CreateRegion(nullptr, PAGE_SIZE);

  void *addr = mmap(nullptr, PAGE_SIZE, PROT_READ | PROT_WRITE, MAP_SHARED, fd.get(), 0);
  ASSERT_TRUE(addr != MAP_FAILED && addr != nullptr);

  EXPECT_THAT(ioctl(fd.get(), ASHMEM_GET_PIN_STATUS, &pin),
              SyscallSucceedsWithValue(ASHMEM_IS_PINNED));

  ASSERT_THAT(munmap(addr, PAGE_SIZE), SyscallSucceeds());
  ASSERT_THAT(close(fd.get()), SyscallSucceeds());
}

// Create a region with two unpinned sub-regions and verify the state
//      [  Unpin   ][        Pin        ][  Unpin   ]
TEST_F(AshmemTest, BasicPinBehavior) {
  ashmem_pin pin_left = {.offset = 0, .len = (uint32_t)PAGE_SIZE};
  ashmem_pin pin_right = {.offset = 3 * (uint32_t)PAGE_SIZE, .len = (uint32_t)PAGE_SIZE};
  ashmem_pin pin_middle = {.offset = (uint32_t)PAGE_SIZE, .len = 2 * (uint32_t)PAGE_SIZE};

  auto fd = CreateRegion(nullptr, 4 * PAGE_SIZE);

  void *addr = mmap(nullptr, 4 * PAGE_SIZE, PROT_READ | PROT_WRITE, MAP_SHARED, fd.get(), 0);
  ASSERT_TRUE(addr != MAP_FAILED && addr != nullptr);

  ASSERT_THAT(ioctl(fd.get(), ASHMEM_UNPIN, &pin_left), SyscallSucceeds());
  ASSERT_THAT(ioctl(fd.get(), ASHMEM_UNPIN, &pin_right), SyscallSucceeds());

  EXPECT_THAT(ioctl(fd.get(), ASHMEM_GET_PIN_STATUS, &pin_left),
              SyscallSucceedsWithValue(ASHMEM_IS_UNPINNED));
  EXPECT_THAT(ioctl(fd.get(), ASHMEM_GET_PIN_STATUS, &pin_middle),
              SyscallSucceedsWithValue(ASHMEM_IS_PINNED));
  EXPECT_THAT(ioctl(fd.get(), ASHMEM_GET_PIN_STATUS, &pin_right),
              SyscallSucceedsWithValue(ASHMEM_IS_UNPINNED));

  ASSERT_THAT(munmap(addr, 4 * PAGE_SIZE), SyscallSucceeds());
  ASSERT_THAT(close(fd.get()), SyscallSucceeds());
}

}  // namespace
