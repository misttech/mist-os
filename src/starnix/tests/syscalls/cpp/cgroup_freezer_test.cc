// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <signal.h>
#include <sys/mount.h>
#include <sys/wait.h>

#include <filesystem>
#include <fstream>
#include <iostream>
#include <string>

#include <gtest/gtest.h>

#include "src/starnix/tests/syscalls/cpp/test_helper.h"

class CgroupFreezerTest : public ::testing::Test {
 public:
  void SetUp() override {
    if (!test_helper::HasSysAdmin()) {
      // From https://docs.kernel.org/admin-guide/cgroup-v2.html#interaction-with-other-namespaces
      // mounting cgroup requires CAP_SYS_ADMIN.
      GTEST_SKIP() << "requires CAP_SYS_ADMIN to mount cgroup";
    }
    create_test_cgroup();
  }

  void TearDown() override { remove_test_cgroup(); }

 protected:
  std::vector<int> test_pids_;

  bool wait_freeze_state_changed(bool is_frozen_state_to_wait) {
    std::ifstream events_file(events_path());
    while (true) {
      if (!events_file.is_open()) {
        return false;
      }

      std::string line;
      while (std::getline(events_file, line)) {
        if (line.starts_with("frozen ")) {
          if (!!std::atoi(line.substr(7).c_str()) == is_frozen_state_to_wait) {
            return true;
          }
        }
      }

      return false;  // "frozen" line not found.
    }
  }

  std::string cgroup_path() { return temp_dir_.path() + "/cgroup"; }
  std::string test_cgroup_path() { return cgroup_path() + "/test"; }
  std::string procs_path() { return test_cgroup_path() + "/cgroup.procs"; }
  std::string freezer_path() { return test_cgroup_path() + "/cgroup.freeze"; }
  std::string events_path() { return test_cgroup_path() + "/cgroup.events"; }

 private:
  void create_test_cgroup() {
    ASSERT_FALSE(std::filesystem::exists(cgroup_path()));
    ASSERT_TRUE(std::filesystem::create_directories(cgroup_path()));
    ASSERT_THAT(mount(nullptr, cgroup_path().c_str(), "cgroup2", 0, nullptr), SyscallSucceeds());
    ASSERT_TRUE(std::filesystem::create_directories(test_cgroup_path()));
  }

  void remove_test_cgroup() {
    // Kill the child processes
    for (int pid : test_pids_) {
      kill(pid, SIGKILL);
      waitpid(pid, NULL, 0);
    }
    if (std::filesystem::exists(test_cgroup_path())) {
      ASSERT_TRUE(std::filesystem::remove(test_cgroup_path()));
    }
    if (std::filesystem::exists(cgroup_path())) {
      if (test_helper::HasSysAdmin()) {
        ASSERT_THAT(umount(cgroup_path().c_str()), SyscallSucceeds());
      }
      ASSERT_TRUE(std::filesystem::remove(cgroup_path()));
    }
  }

  test_helper::ScopedTempDir temp_dir_;
};

TEST_F(CgroupFreezerTest, FreezeSingleProcess) {
  pid_t parent_pid = getpid();
  test_helper::ForkHelper fork_helper;

  pid_t child_pid = fork_helper.RunInForkedProcess([parent_pid] {
    // Set up a signal set to wait for SIGUSR1 which will be sent by the child process
    test_helper::SignalMaskHelper mask_helper;
    mask_helper.blockSignal(SIGUSR1);

    while (true) {
      // Wait for SIGUSR1
      mask_helper.waitForSignal(SIGUSR1);
      kill(parent_pid, SIGUSR1);
    }
  });
  test_pids_.push_back(child_pid);

  // Write the child PID to the cgroup
  std::ofstream procs_file(procs_path());
  procs_file << child_pid;
  procs_file.close();

  // Set up a signal set to wait for SIGUSR1 which will be sent by the child process
  test_helper::SignalMaskHelper mask_helper;
  mask_helper.blockSignal(SIGUSR1);

  // Send signal; child should receive it.
  kill(child_pid, SIGUSR1);
  mask_helper.waitForSignal(SIGUSR1);

  // Freeze the cgroup
  std::ofstream freeze_file(freezer_path());
  freeze_file << 1;
  freeze_file.close();
  ASSERT_TRUE(wait_freeze_state_changed(true));

  // Send signal; frozen child should *not* receive it.
  kill(child_pid, SIGUSR1);

  // Set up a time limit for sigtimedwait. Should timeout without receiving the signal.
  EXPECT_THAT(mask_helper.timedWaitForSignal(SIGUSR1, 2), SyscallFailsWithErrno(EAGAIN));

  // Unfreeze the child process
  freeze_file.open(freezer_path());
  freeze_file << 0;
  freeze_file.close();
  EXPECT_TRUE(wait_freeze_state_changed(false));

  // Child will process the last signal after thawed.
  mask_helper.waitForSignal(SIGUSR1);

  // Kill the child process
  EXPECT_EQ(0, kill(child_pid, SIGKILL));
  // Wait for the child process to terminate
  fork_helper.WaitForChildren();
}
