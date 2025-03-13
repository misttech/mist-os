// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fit/defer.h>
#include <stdio.h>
#include <sys/reboot.h>
#include <sys/wait.h>
#include <unistd.h>

int main(int argc, char** argv) {
  auto reboot_before_exit = fit::defer([] { reboot(RB_POWER_OFF); });

  pid_t child_pid = fork();
  if (child_pid == -1) {
    perror("fork() failed");
    return 1;
  }

  if (child_pid == 0) {
    execv(argv[1], argv + 1);
    perror("exec failed");
    exit(1);
  }

  int wstatus;
  if (waitpid(child_pid, &wstatus, 0) == -1) {
    perror("waitpid() failed");
    return 1;
  }
  if (WIFEXITED(wstatus) && WEXITSTATUS(wstatus) == 0) {
    fprintf(stderr, "TEST SUCCESS\n");
  } else {
    fprintf(stderr, "TEST FAILURE\n");
  }

  return 0;
}
