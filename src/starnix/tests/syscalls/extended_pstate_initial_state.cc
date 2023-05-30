// Copyright 2023 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

#include <gtest/gtest.h>

#include "src/lib/files/file.h"
#include "src/lib/files/path.h"
#include "src/starnix/tests/syscalls/test_helper.h"

TEST(ExtendedPstateInitialState, Basic) {
  // When running in Starnix the child binary is mounted at this path in the test's namespace.
  std::string child_path = "data/tests/extended_pstate_initial_state_child";
  if (!files::IsFile(child_path)) {
    // When running on host the child binary is next to the test binary.
    char self_path[PATH_MAX];
    realpath("/proc/self/exe", self_path);

    child_path =
        files::JoinPath(files::GetDirectoryName(self_path), "extended_pstate_initial_state_child");
  }
  ASSERT_TRUE(files::IsFile(child_path)) << child_path;
  ForkHelper helper;
  helper.RunInForkedProcess([&child_path] {
    char* argv[] = {nullptr};
    char* envp[] = {nullptr};
    ASSERT_EQ(execve(child_path.c_str(), argv, envp), 0)
        << "execve error: " << errno << " (" << strerror(errno) << ")";
  });
}
