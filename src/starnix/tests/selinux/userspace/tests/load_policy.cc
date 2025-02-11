// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <sys/xattr.h>
#include <unistd.h>

#include <string>

#include <gtest/gtest.h>

#include "src/starnix/tests/selinux/userspace/util.h"

// Linux inserts a mysterious '\0' at the end of the label in /proc/<pid>/attr/current, SEStarnix
// currently doesn't.
std::string RemoveTailNull(std::string in) {
  if (in.size() > 0 && in[in.size() - 1] == 0) {
    in.pop_back();
  }
  return in;
}

void RunTest() {
  LoadPolicy("minimal_policy.pp");
  // Apparently Linux and Starnix disagree on the initial label for our process, but that may come
  // from being launched from different contexts.
  WriteContents("/proc/thread-self/attr/current", "system_u:unconfined_r:unconfined_t:s0");

  std::string s = ReadFile("/proc/thread-self/attr/current");
  EXPECT_EQ(RemoveTailNull(ReadFile("/proc/thread-self/attr/current")),
            "system_u:unconfined_r:unconfined_t:s0");
}
