// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <time.h>

#include "src/proc/tests/chromiumos/syscalls/task_test.h"

// This simple program is used to exec for the Task.Clone3Vfork test. That test blocks until
// this child process has executed. To help ensure that the test actually waits for this process'
// completion, this process does a short sleep.
int main(int argc, char** argv) {
  struct timespec request = {.tv_sec = 0, .tv_nsec = kCloneVforkSleepNS};
  nanosleep(&request, nullptr);
  return 0;
}
