// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <sys/ioctl.h>
#include <sys/xattr.h>
#include <unistd.h>

#include <string>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>
#include <linux/fs.h>

#include "src/starnix/tests/selinux/userspace/util.h"

namespace {

constexpr char kTestFilePathTemplate[] = "/tmp/ioctl_test_file:XXXXXX";
constexpr char kTestFileSecurityContext[] = "test_u:object_r:test_ioctl_file_t:s0";
constexpr char kSelinuxXattr[] = "security.selinux";

fit::result<int, fbl::unique_fd> CreateTestFile() {
  std::string filename(kTestFilePathTemplate);
  fbl::unique_fd fd(mkstemp(filename.data()));
  if (!fd.is_valid()) {
    return fit::error(errno);
  }
  if (fsetxattr(fd.get(), kSelinuxXattr, kTestFileSecurityContext, sizeof(kTestFileSecurityContext),
                0) < 0) {
    return fit::error(errno);
  }
  return fit::ok(std::move(fd));
}

fit::result<int, int> GetBlockSize(int fd) {
  int block_size = 0;
  int result = ioctl(fd, FIGETBSZ, &block_size);
  if (result < 0) {
    return fit::error(errno);
  }
  return fit::ok(result);
}

TEST(IoctlTest, BasicIoctlAllowed) {
  LoadPolicy("ioctl_policy.pp");

  auto enforce = ScopedEnforcement::SetEnforcing();

  auto test_fd = CreateTestFile();
  ASSERT_TRUE(test_fd.is_ok());

  constexpr char kTestSecurityContext[] = "test_u:test_r:test_ioctl_figetbsz_allowed_t:s0";
  ASSERT_TRUE(RunAs(kTestSecurityContext, [&] {
    auto result = GetBlockSize(test_fd.value().get());
    EXPECT_TRUE(result.is_ok());
  }));
}

TEST(IoctlTest, BasicIoctlDenied) {
  LoadPolicy("ioctl_policy.pp");

  auto enforce = ScopedEnforcement::SetEnforcing();

  auto test_fd = CreateTestFile();
  ASSERT_TRUE(test_fd.is_ok());

  constexpr char kTestSecurityContext[] = "test_u:test_r:test_ioctl_figetbsz_denied_t:s0";
  ASSERT_TRUE(RunAs(kTestSecurityContext, [&] {
    auto result = GetBlockSize(test_fd.value().get());
    EXPECT_TRUE(result.is_error());
  }));
}

}  // namespace
