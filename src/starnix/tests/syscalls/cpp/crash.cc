// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

#include <gtest/gtest.h>

TEST(CrashTest, Crash) {
  pid_t child = fork();
  if (child == 0) {
    *((volatile char*)0x0) = 0;
  } else {
    int wstatus = 0;
    int options = 0;
    EXPECT_EQ(waitpid(child, &wstatus, options), child);
    EXPECT_TRUE(!WIFEXITED(wstatus)) << wstatus;
    EXPECT_TRUE(WIFSIGNALED(wstatus)) << wstatus;
    EXPECT_EQ(WTERMSIG(wstatus), SIGSEGV);
  }
}
