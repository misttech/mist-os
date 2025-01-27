// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/types.h>
#include <sys/utsname.h>
#include <sys/xattr.h>
#include <syscall.h>
#include <unistd.h>

#include <cerrno>
#include <csignal>
#include <cstddef>
#include <cstdint>
#include <cstdlib>
#include <functional>
#include <optional>

#include <linux/userfaultfd.h>

#include "gtest/gtest.h"
#include "src/lib/files/file.h"
#include "src/starnix/tests/syscalls/cpp/proc_test_base.h"
#include "src/starnix/tests/syscalls/cpp/test_helper.h"

#ifndef MREMAP_DONTUNMAP
#define MREMAP_DONTUNMAP 4
#endif

#ifndef UFFD_USER_MODE_ONLY
#define UFFD_USER_MODE_ONLY 1
#endif

class UffdWrapper {
 public:
  UffdWrapper() {
    int flags = O_CLOEXEC | O_NONBLOCK;
    // UFFD_USER_MODE_ONLY was introduced in 5.11 and restricts page fault handling to faults
    // originated from user space only. Without this flag, additional privileges might be needed to
    // run these tests.
    if (test_helper::IsKernelVersionAtLeast(5, 11) || test_helper::IsStarnix()) {
      flags |= UFFD_USER_MODE_ONLY;
    }
    uffd_ = SAFE_SYSCALL(static_cast<int>(syscall(__NR_userfaultfd, flags)));
  }

  ~UffdWrapper() { SAFE_SYSCALL(close(uffd_)); }

  void api() const { api(0); }

  void api(uint64_t features) const {
    uffdio_api api = {.api = UFFD_API, .features = features, .ioctls = 0};
    SAFE_SYSCALL(ioctl(uffd_, UFFDIO_API, &api));
  }

  void register_range(uintptr_t start, size_t len, size_t mode) const {
    uffdio_register uffdio_register;
    uffdio_register.range.start = start;
    uffdio_register.range.len = len;
    uffdio_register.mode = mode;

    ASSERT_EQ(ioctl(uffd_, UFFDIO_REGISTER, &uffdio_register), 0);
  }

  int unregister_range(uintptr_t start, size_t len) const {
    uffdio_range uffdio_range;
    uffdio_range.start = start;
    uffdio_range.len = len;

    return ioctl(uffd_, UFFDIO_UNREGISTER, &uffdio_range);
  }

  void copy_one_page(uintptr_t src, uintptr_t dst) const {
    const size_t page_size = SAFE_SYSCALL(sysconf(_SC_PAGE_SIZE));
    uffdio_copy uffdio_copy{
        .dst = dst & ~(page_size - 1),  // Align to page size
        .src = src,
        .len = page_size,
        .mode = 0,
        .copy = 0,
    };
    ASSERT_EQ(ioctl(uffd_, UFFDIO_COPY, &uffdio_copy), 0);
  }

  int fd() const { return uffd_; }

 private:
  int uffd_;
};

TEST(UffdTest, NoOperationsBeforeInit) {
  const size_t page_size = SAFE_SYSCALL(sysconf(_SC_PAGE_SIZE));

  auto full_range = ASSERT_RESULT_SUCCESS_AND_RETURN(test_helper::ScopedMMap::MMap(
      nullptr, page_size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0));
  uffdio_range uffdio_range{.start = reinterpret_cast<uintptr_t>(full_range.mapping()),
                            .len = page_size};

  UffdWrapper uffd;

  // No other ioctl is permitted on this file descriptor until the UFFDIO_API is called for the
  // first time
  uffdio_register uffdio_register{
      .range = uffdio_range, .mode = UFFDIO_REGISTER_MODE_MISSING, .ioctls = 0};
  EXPECT_NE(ioctl(uffd.fd(), UFFDIO_REGISTER, &uffdio_register), 0);
  EXPECT_EQ(errno, EINVAL);
  EXPECT_NE(ioctl(uffd.fd(), UFFDIO_UNREGISTER, &uffdio_range), 0);
  EXPECT_EQ(errno, EINVAL);

  uffdio_api api = {.api = UFFD_API, .features = 0, .ioctls = 0};
  ASSERT_EQ(ioctl(uffd.fd(), UFFDIO_API, &api), 0);

  // The same ioctls succeed now
  EXPECT_EQ(ioctl(uffd.fd(), UFFDIO_REGISTER, &uffdio_register), 0);
  EXPECT_EQ(ioctl(uffd.fd(), UFFDIO_UNREGISTER, &uffdio_range), 0);

  // Reinitialization is not allowed
  ASSERT_NE(ioctl(uffd.fd(), UFFDIO_API, &api), 0);
  EXPECT_EQ(errno, EPERM);
}

class UffdProcTest : public ProcTestBase {
 public:
  std::optional<test_helper::MemoryMappingExt> FindMemoryMappingInSmaps(uintptr_t addr) const {
    std::string smaps;
    if (!files::ReadFileToString(proc_path() + "/self/smaps", &smaps)) {
      return std::nullopt;
    }
    return test_helper::find_memory_mapping_ext(addr, smaps);
  }
};

TEST_F(UffdProcTest, Register) {
  const size_t page_size = SAFE_SYSCALL(sysconf(_SC_PAGE_SIZE));
  size_t len = page_size;

  UffdWrapper uffd;
  uffd.api();

  auto full_range = ASSERT_RESULT_SUCCESS_AND_RETURN(test_helper::ScopedMMap::MMap(
      nullptr, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0));
  intptr_t start = reinterpret_cast<intptr_t>(full_range.mapping());

  uffd.register_range(start, len, UFFDIO_REGISTER_MODE_MISSING);

  auto registered_mapping = FindMemoryMappingInSmaps(start);
  ASSERT_TRUE(registered_mapping->ContainsFlag("um"));
}

TEST_F(UffdProcTest, RegisterPart) {
  const size_t page_size = SAFE_SYSCALL(sysconf(_SC_PAGE_SIZE));
  size_t len = 3 * page_size;

  UffdWrapper uffd;
  uffd.api();

  auto full_range = ASSERT_RESULT_SUCCESS_AND_RETURN(test_helper::ScopedMMap::MMap(
      nullptr, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0));
  intptr_t start = reinterpret_cast<intptr_t>(full_range.mapping());

  uffd.register_range(start + page_size, page_size, UFFDIO_REGISTER_MODE_MISSING);

  auto page1 = FindMemoryMappingInSmaps(start);
  ASSERT_FALSE(page1->ContainsFlag("um"));
  auto page2 = FindMemoryMappingInSmaps(start + page_size);
  ASSERT_TRUE(page2->ContainsFlag("um"));
  ASSERT_EQ(page2->start, start + page_size);
  ASSERT_EQ(page2->end, start + (2 * page_size));
  auto page3 = FindMemoryMappingInSmaps(start + (2 * page_size));
  ASSERT_FALSE(page3->ContainsFlag("um"));
}

TEST(UffdTest, RegisterEmpty) {
  const size_t page_size = SAFE_SYSCALL(sysconf(_SC_PAGE_SIZE));

  UffdWrapper uffd;
  uffd.api();

  // Identify an empty page by mapping and then unmapping
  auto full_range = ASSERT_RESULT_SUCCESS_AND_RETURN(test_helper::ScopedMMap::MMap(
      nullptr, page_size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0));
  intptr_t start = reinterpret_cast<intptr_t>(full_range.mapping());
  full_range.Unmap();

  uffdio_register uffdio_register;
  uffdio_register.range.start = start;
  uffdio_register.range.len = page_size;
  uffdio_register.mode = UFFDIO_REGISTER_MODE_MISSING;

  ASSERT_NE(ioctl(uffd.fd(), UFFDIO_REGISTER, &uffdio_register), 0);
  EXPECT_EQ(errno, EINVAL);
}

TEST(UffdTest, TwoUffdCantOverlap) {
  const size_t page_size = SAFE_SYSCALL(sysconf(_SC_PAGE_SIZE));
  size_t len = page_size;

  auto full_range = ASSERT_RESULT_SUCCESS_AND_RETURN(test_helper::ScopedMMap::MMap(
      nullptr, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0));
  intptr_t start = reinterpret_cast<intptr_t>(full_range.mapping());
  uffdio_register uffdio_register;

  // Register the mapping with uffd_a
  UffdWrapper uffd_a;
  uffd_a.api();
  uffdio_register.range.start = start;
  uffdio_register.range.len = len;
  uffdio_register.mode = UFFDIO_REGISTER_MODE_MISSING;
  ASSERT_EQ(ioctl(uffd_a.fd(), UFFDIO_REGISTER, &uffdio_register), 0);

  // Can't register the same mapping with a different uffd
  UffdWrapper uffd_b;
  uffd_b.api();
  uffdio_register.range.start = start;
  uffdio_register.range.len = page_size;
  uffdio_register.mode = UFFDIO_REGISTER_MODE_MISSING;
  ASSERT_NE(ioctl(uffd_b.fd(), UFFDIO_REGISTER, &uffdio_register), 0);
  EXPECT_EQ(errno, EBUSY);
}

TEST_F(UffdProcTest, SmapsOnFork) {
  const size_t page_size = SAFE_SYSCALL(sysconf(_SC_PAGE_SIZE));
  size_t len = page_size;

  UffdWrapper uffd;
  uffd.api();

  auto full_range = ASSERT_RESULT_SUCCESS_AND_RETURN(test_helper::ScopedMMap::MMap(
      nullptr, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0));
  intptr_t start = reinterpret_cast<intptr_t>(full_range.mapping());

  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([&] {
    uffd.register_range(start, len, UFFDIO_REGISTER_MODE_MISSING);

    // The registration isn't reflected in child's smaps
    auto registered_mapping = FindMemoryMappingInSmaps(start);
    ASSERT_FALSE(registered_mapping->ContainsFlag("um"));
  });
  ASSERT_TRUE(helper.WaitForChildren());

  // Instead, it is reflected in the parent's smaps
  auto registered_mapping = FindMemoryMappingInSmaps(start);
  ASSERT_TRUE(registered_mapping->ContainsFlag("um"));
}

TEST_F(UffdProcTest, Unregister) {
  const size_t page_size = SAFE_SYSCALL(sysconf(_SC_PAGE_SIZE));
  size_t len = 2 * page_size;
  UffdWrapper uffd_a;
  uffd_a.api(UFFD_FEATURE_SIGBUS);
  UffdWrapper uffd_b;
  uffd_b.api(UFFD_FEATURE_SIGBUS);

  auto full_range = ASSERT_RESULT_SUCCESS_AND_RETURN(test_helper::ScopedMMap::MMap(
      nullptr, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0));
  intptr_t start = reinterpret_cast<intptr_t>(full_range.mapping());
  reinterpret_cast<char *>(full_range.mapping())[0] = 'a';

  EXPECT_EQ(uffd_a.unregister_range(start, len), 0);
  uffd_a.register_range(start, page_size, UFFDIO_REGISTER_MODE_MISSING);
  EXPECT_EQ(uffd_a.unregister_range(start, len), 0);
  uffd_b.register_range(start, page_size, UFFDIO_REGISTER_MODE_MISSING);
  uffd_a.register_range(start + page_size, page_size, UFFDIO_REGISTER_MODE_MISSING);
  // Unregistration affects all uffd registrations in the range, regardless of which uffd they
  // belong to.
  EXPECT_EQ(uffd_a.unregister_range(start, len), 0);

  auto registered_mapping_a = FindMemoryMappingInSmaps(start);
  ASSERT_FALSE(registered_mapping_a->ContainsFlag("um"));
  auto registered_mapping_b = FindMemoryMappingInSmaps(start + page_size);
  ASSERT_FALSE(registered_mapping_b->ContainsFlag("um"));
}

TEST_F(UffdProcTest, Close) {
  const size_t page_size = SAFE_SYSCALL(sysconf(_SC_PAGE_SIZE));
  size_t len = 2 * page_size;

  auto full_range = ASSERT_RESULT_SUCCESS_AND_RETURN(test_helper::ScopedMMap::MMap(
      nullptr, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0));
  intptr_t start = reinterpret_cast<intptr_t>(full_range.mapping());
  memset(full_range.mapping(), 'a', len);
  UffdWrapper uffd_a;
  uffd_a.api(UFFD_FEATURE_SIGBUS);
  {
    UffdWrapper uffd_b;
    uffd_b.api(UFFD_FEATURE_SIGBUS);

    uffd_a.register_range(start, page_size, UFFDIO_REGISTER_MODE_MISSING);
    uffd_b.register_range(start + page_size, page_size, UFFDIO_REGISTER_MODE_MISSING);
  }

  auto registered_mapping_a = FindMemoryMappingInSmaps(start);
  auto registered_mapping_b = FindMemoryMappingInSmaps(start + page_size);
  ASSERT_NE(registered_mapping_a->start, registered_mapping_b->start);
  ASSERT_TRUE(registered_mapping_a->ContainsFlag("um"));
  ASSERT_FALSE(registered_mapping_b->ContainsFlag("um"));
}
