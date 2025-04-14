// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <sys/xattr.h>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "src/starnix/tests/selinux/userspace/util.h"

namespace {

TEST(InitialSidTest, NewFileTakesFilesystemInitialSidType) {
  LoadPolicy("initial_sids_policy.pp");
  ASSERT_EQ(WriteTaskAttr("current", "unconfined_u:unconfined_r:unconfined_t:s0"), fit::ok());

  // Create a file in `/`, which is not specifically labeled in the policy and will default to being
  // labeled with the `unlabeled` initial SID.
  auto fd = fbl::unique_fd(open("/test-file", O_CREAT, 0777));
  ASSERT_THAT(fd.get(), SyscallSucceeds()) << "while creating file";

  ssize_t len = 0;
  char label[256] = {};
  ASSERT_THAT(len = fgetxattr(fd.get(), "security.selinux", label, sizeof(label)),
              SyscallSucceeds());
  EXPECT_EQ(RemoveTrailingNul(std::string(label, len)),
            "unconfined_u:object_r:unlabeled_sid_test_t:s0");
}

}  // namespace
