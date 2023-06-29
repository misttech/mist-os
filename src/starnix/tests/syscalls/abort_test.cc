// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

#include <thread>

#include <gtest/gtest.h>

#include "src/starnix/tests/syscalls/test_helper.h"

TEST(AbortTest, Abort) {
  ForkHelper helper;
  helper.ExpectSignal(SIGABRT);
  helper.OnlyWaitForForkedChildren();
  helper.RunInForkedProcess([] { abort(); });
  ASSERT_TRUE(helper.WaitForChildren());
}

TEST(AbortTest, AbortFromChildThread) {
  ForkHelper helper;
  helper.ExpectSignal(SIGABRT);
  helper.OnlyWaitForForkedChildren();
  helper.RunInForkedProcess([] {
    volatile bool done = false;
    std::thread child_thread([] { abort(); });
    // Spin in userspace forever to verify that we can kick out of restricted
    // mode even when not executing any syscalls.
    while (!done) {
      // This loop issues a volatile load on each iteration which is considered
      // progress by C++ even though it has no practical effect:
      // http://eel.is/c++draft/basic.exec#intro.progress
    }
    ASSERT_TRUE(false) << "Should not be reached";
  });
  ASSERT_TRUE(helper.WaitForChildren());
}
