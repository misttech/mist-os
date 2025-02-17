// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <sys/xattr.h>
#include <unistd.h>

#include <string>

#include <gtest/gtest.h>

#include "src/starnix/tests/selinux/userspace/util.h"

// Under the default policy, a pipe is labeled with the label of its creating process.
void RunTest() {
  int pipe_before_policy[2];
  EXPECT_THAT(pipe(pipe_before_policy), SyscallSucceeds());
  LoadPolicy("minimal_policy.pp");

  ssize_t len = 0;
  char label[256] = {};
  EXPECT_NE((len = fgetxattr(pipe_before_policy[0], "security.selinux", label, sizeof(label))), -1);
  // TODO: https://fxbug.dev/395625171 - This ignores the final '\0' added by Linux.
  EXPECT_EQ(std::string(label, len).c_str(), std::string("system_u:unconfined_r:unconfined_t:s0"));

  WriteContents("/proc/thread-self/attr/current", "system_u:unconfined_r:unconfined_t:s0");

  int pipe_after_policy[2];
  EXPECT_THAT(pipe(pipe_after_policy), SyscallSucceeds());

  len = 0;
  EXPECT_THAT((len = fgetxattr(pipe_after_policy[0], "security.selinux", label, sizeof(label))),
              SyscallSucceeds());
  // TODO: https://fxbug.dev/395625171 - This ignores the final '\0' added by Linux.
  EXPECT_EQ(std::string(label, len).c_str(), std::string("system_u:unconfined_r:unconfined_t:s0"));
}
