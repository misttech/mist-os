// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <errno.h>
#include <fcntl.h>
#include <lib/fit/defer.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#include <gtest/gtest.h>

#include "src/starnix/tests/syscalls/cpp/proc_test_base.h"
#include "src/starnix/tests/syscalls/cpp/test_helper.h"

namespace {

class ProcSelfMemProts : public ProcTestBase, public ::testing::WithParamInterface<int> {};

TEST_P(ProcSelfMemProts, CanWriteToPrivateAnonymousMappings) {
  if (access("/proc/self/mem", R_OK | W_OK) == -1) {
    // Host tests run with read-only /proc, so we can't run this test there.
    // See: https://fxbug.dev/328301908
    // TODO(https://fxbug.dev/317285180) don't skip on baseline
    GTEST_SKIP() << "Cannot write to /proc/self/mem";
  }

  uint8_t buf[16] = {0};
  int prot = GetParam();

  const size_t page_size = SAFE_SYSCALL(sysconf(_SC_PAGE_SIZE));
  void* mapped = mmap(nullptr, page_size, prot, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  ASSERT_NE(mapped, MAP_FAILED) << "mmap: " << std::strerror(errno);
  auto cleanup = fit::defer([mapped, page_size]() { EXPECT_EQ(munmap(mapped, page_size), 0); });

  fbl::unique_fd fd = fbl::unique_fd(open("/proc/self/mem", O_RDWR));
  ASSERT_TRUE(fd.is_valid()) << "open /proc/self/mem: " << std::strerror(errno);

  const off64_t offset = static_cast<off64_t>(reinterpret_cast<uintptr_t>(mapped));
  ASSERT_EQ(lseek64(fd.get(), offset, SEEK_SET), offset) << "lseek: " << std::strerror(errno);

  memset(buf, 'a', sizeof(buf));

  ssize_t n = write(fd.get(), buf, sizeof(buf));
  EXPECT_NE(n, -1) << "write: " << std::strerror(errno);
  EXPECT_EQ(static_cast<size_t>(n), sizeof(buf));

  ASSERT_EQ(mprotect(mapped, page_size, PROT_READ), 0) << "mprotect: " << std::strerror(errno);
  EXPECT_EQ(memcmp(mapped, buf, sizeof(buf)), 0);
}

inline std::string ProtToString(const testing::TestParamInfo<int>& info) {
  std::string prot = "";
  if (info.param == PROT_NONE) {
    return "None";
  }
  if (info.param & PROT_READ) {
    prot += "Read";
  }
  if (info.param & PROT_WRITE) {
    prot += "Write";
  }
  if (info.param & PROT_EXEC) {
    prot += "Execute";
  }
  return prot;
}

INSTANTIATE_TEST_SUITE_P(/* no prefix */, ProcSelfMemProts,
                         ::testing::Values(PROT_NONE, PROT_READ, PROT_WRITE, PROT_EXEC,
                                           PROT_READ | PROT_WRITE, PROT_READ | PROT_EXEC,
                                           PROT_WRITE | PROT_EXEC,
                                           PROT_READ | PROT_WRITE | PROT_EXEC),
                         ProtToString);

}  // namespace
