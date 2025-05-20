// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <sys/xattr.h>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "src/starnix/tests/selinux/userspace/base_initial_sids.h"
#include "src/starnix/tests/selinux/userspace/util.h"

extern std::string DoPrePolicyLoadWork() { return "file_transition_policy.pp"; }

namespace {

TEST(UnlabeledFsTest, CreateFileInUnlabeledFs) {
  ASSERT_EQ(WriteTaskAttr("current", "unconfined_u:unconfined_r:unconfined_t:s0"), fit::ok());

  // Check that `/` which is not labeled in the policy is labeled with the `unlabeled` initial SID.
  EXPECT_EQ(GetLabel("/"), kUnlabeledInitialSid);

  // Check that creating a file in an unlabeled filesystem follows transition rules.
  auto fd = fbl::unique_fd(open("/test-file", O_CREAT, 0777));
  ASSERT_THAT(fd.get(), SyscallSucceeds()) << "while creating file";
  EXPECT_EQ(GetLabel(fd.get()), "unconfined_u:object_r:unlabeled_unconfined_file_t:s0");
}

}  // namespace
