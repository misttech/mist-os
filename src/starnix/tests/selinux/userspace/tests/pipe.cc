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
  LoadPolicy("minimal_policy.pp");

  WriteContents("/proc/thread-self/attr/current", "system_u:object_r:unlabeled_t:s0");

  int ends[2];
  EXPECT_THAT(pipe(ends), SyscallSucceeds());

  char label[256] = {};
  // TODO: https://fxbug.dev/395625171 - This ignores the final '\0' added by Linux. Instead, we
  // ensure that zero-initialized, leave a '\0' at the end and treat the string as null-terminated.
  EXPECT_THAT(fgetxattr(ends[0], "security.selinux", label, sizeof(label) - 1), SyscallSucceeds());
  EXPECT_EQ(std::string(label), "system_u:object_r:unlabeled_t:s0");
}
