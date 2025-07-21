// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <signal.h>

#include <gtest/gtest.h>

#include "src/starnix/tests/syscalls/cpp/syscall_matchers.h"

// For a given list of signal numbers, checks whether the signal disposition
// for each one is SIG_DFL or SIG_IGN.
//
// Arguments should be provided in pairs, with each signal number followed by
// either "default" or "ignore".
int main(int argc, char** argv) {
  EXPECT_EQ(argc % 2, 1);
  if (::testing::Test::HasFailure()) {
    return 1;
  }

  for (int i = 1; i < (argc - 1); i += 2) {
    int signum = std::atoi(argv[i]);
    auto action = std::string(argv[i + 1]);

    struct sigaction sa{};
    EXPECT_THAT(sigaction(signum, NULL, &sa), SyscallSucceeds());
    EXPECT_TRUE(action == "default" || action == "ignore");
    if (::testing::Test::HasFailure()) {
      return 1;
    }
    auto expected = (action == "default") ? SIG_DFL : SIG_IGN;
    EXPECT_EQ(sa.sa_handler, expected);
  }

  return ::testing::Test::HasFailure() ? 1 : 0;
}
