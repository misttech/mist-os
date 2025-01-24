// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <signal.h>
#include <sys/mount.h>
#include <sys/wait.h>

#include <deque>
#include <filesystem>
#include <fstream>
#include <string>

#include <gtest/gtest.h>

#include "src/lib/files/file.h"
#include "src/starnix/tests/syscalls/cpp/test_helper.h"

namespace {

constexpr char PROCS_FILE[] = "cgroup.procs";
constexpr char FREEZE_FILE[] = "cgroup.freeze";
constexpr char EVENTS_FILE[] = "cgroup.events";

std::string procs_path(const std::string& cgroup_path) { return cgroup_path + "/" + PROCS_FILE; }
std::string freeze_path(const std::string& cgroup_path) { return cgroup_path + "/" + FREEZE_FILE; }
std::string events_path(const std::string& cgroup_path) { return cgroup_path + "/" + EVENTS_FILE; }

}  // namespace

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

  static bool wait_freeze_state_changed(std::string event_path, bool is_frozen_state_to_wait) {
    while (true) {
      std::ifstream events_file(event_path);
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
    }
  }

  void create_child_cgroup(const std::string& child_path) {
    ASSERT_TRUE(std::filesystem::create_directories(child_path));
    cgroups_.push_front(child_path);
  }

  std::string cgroup_path() { return temp_dir_.path() + "/cgroup"; }
  std::string test_cgroup_path() { return cgroup_path() + "/test"; }

 private:
  void create_test_cgroup() {
    ASSERT_FALSE(std::filesystem::exists(cgroup_path()));
    ASSERT_TRUE(std::filesystem::create_directories(cgroup_path()));
    ASSERT_THAT(mount(nullptr, cgroup_path().c_str(), "cgroup2", 0, nullptr), SyscallSucceeds());
    ASSERT_TRUE(std::filesystem::create_directories(test_cgroup_path()));
    cgroups_.push_front(test_cgroup_path());
  }

  void remove_test_cgroup() {
    // Kill the child processes
    for (int pid : test_pids_) {
      kill(pid, SIGKILL);
      waitpid(pid, NULL, 0);
    }
    for (const auto& cgroup : cgroups_) {
      if (std::filesystem::exists(cgroup)) {
        ASSERT_TRUE(std::filesystem::remove(cgroup));
      }
    }
    if (std::filesystem::exists(cgroup_path())) {
      if (test_helper::HasSysAdmin()) {
        ASSERT_THAT(umount(cgroup_path().c_str()), SyscallSucceeds());
      }
      ASSERT_TRUE(std::filesystem::remove(cgroup_path()));
    }
  }

  test_helper::ScopedTempDir temp_dir_;
  std::deque<std::string> cgroups_;
};

TEST_F(CgroupFreezerTest, FreezeFileAccess) {
  std::string freeze_str;
  EXPECT_TRUE(files::ReadFileToString(freeze_path(test_cgroup_path()), &freeze_str));
  EXPECT_EQ(freeze_str, "0\n");

  EXPECT_TRUE(files::WriteFile(freeze_path(test_cgroup_path()), "1"));
  EXPECT_TRUE(files::ReadFileToString(freeze_path(test_cgroup_path()), &freeze_str));
  EXPECT_EQ(freeze_str, "1\n");

  EXPECT_TRUE(files::WriteFile(freeze_path(test_cgroup_path()), "0"));
  EXPECT_TRUE(files::ReadFileToString(freeze_path(test_cgroup_path()), &freeze_str));
  EXPECT_EQ(freeze_str, "0\n");
}

TEST_F(CgroupFreezerTest, FreezeSingleProcess) {
  pid_t parent_pid = getpid();
  test_helper::ForkHelper fork_helper;

  // Set up a signal set to wait for SIGUSR1 which will be sent by the child process
  test_helper::SignalMaskHelper mask_helper;
  mask_helper.blockSignal(SIGUSR1);

  pid_t child_pid = fork_helper.RunInForkedProcess([parent_pid] {
    // Set up a signal set to wait for SIGUSR1 which will be sent by the parent process
    test_helper::SignalMaskHelper mask_helper;
    mask_helper.blockSignal(SIGUSR1);

    // Wait for the first SIGUSR1 before freeze
    mask_helper.waitForSignal(SIGUSR1);
    kill(parent_pid, SIGUSR1);

    // Wait for the second SIGUSR1 after freeze
    mask_helper.waitForSignal(SIGUSR1);
    kill(parent_pid, SIGUSR1);
  });
  printf("Test proc (%d) folked child (%d)\n", parent_pid, child_pid);
  test_pids_.push_back(child_pid);

  // Write the child PID to the cgroup
  files::WriteFile(procs_path(test_cgroup_path()), std::to_string(child_pid));

  // Send signal; child should receive it.
  kill(child_pid, SIGUSR1);
  mask_helper.waitForSignal(SIGUSR1);

  // Freeze the cgroup
  files::WriteFile(freeze_path(test_cgroup_path()), "1");

  // Send signal; frozen child should *not* receive it.
  kill(child_pid, SIGUSR1);

  // Set up a time limit for sigtimedwait. Should timeout without receiving the signal.
  EXPECT_THAT(mask_helper.timedWaitForSignal(SIGUSR1, 1), SyscallFailsWithErrno(EAGAIN));

  // Unfreeze the child process
  files::WriteFile(freeze_path(test_cgroup_path()), "0");

  // Child will process the last signal after thawed.
  mask_helper.waitForSignal(SIGUSR1);

  // Wait for the child process to terminate
  EXPECT_TRUE(fork_helper.WaitForChildren());
}

TEST_F(CgroupFreezerTest, SIGKILLAfterFrozen) {
  pid_t parent_pid = getpid();
  test_helper::ForkHelper fork_helper;

  // Set up a signal set to wait for SIGUSR1 which will be sent by the child process
  test_helper::SignalMaskHelper mask_helper;
  mask_helper.blockSignal(SIGUSR1);

  pid_t child_pid = fork_helper.RunInForkedProcess([parent_pid] {
    // Set up a signal set to wait for SIGUSR1 which will be sent by the parent process
    test_helper::SignalMaskHelper mask_helper;
    mask_helper.blockSignal(SIGUSR1);

    // Notify the parent that the child starts running.
    mask_helper.waitForSignal(SIGUSR1);
    kill(parent_pid, SIGUSR1);

    // Wait for the SIGUSR1 that should never be received.
    mask_helper.waitForSignal(SIGUSR1);
    kill(parent_pid, SIGUSR1);
  });
  test_pids_.push_back(child_pid);

  // Make sure the child starts running.
  kill(child_pid, SIGUSR1);
  mask_helper.waitForSignal(SIGUSR1);

  // Write the child PID to the cgroup
  files::WriteFile(procs_path(test_cgroup_path()), std::to_string(child_pid));

  // Freeze the cgroup
  files::WriteFile(freeze_path(test_cgroup_path()), "1");

  // Send signal; frozen child should *not* receive it.
  kill(child_pid, SIGUSR1);

  // Set up a time limit for sigtimedwait. Should timeout without receiving the signal.
  EXPECT_THAT(mask_helper.timedWaitForSignal(SIGUSR1, 1), SyscallFailsWithErrno(EAGAIN));

  // Kill the child process without thawing
  EXPECT_EQ(0, kill(child_pid, SIGKILL));
  // Wait for the child process to terminate
  EXPECT_FALSE(fork_helper.WaitForChildren());
}

TEST_F(CgroupFreezerTest, AddProcAfterFrozen) {
  pid_t parent_pid = getpid();
  test_helper::ForkHelper fork_helper;

  // Set up a signal set to wait for SIGUSR1 which will be sent by the child process
  test_helper::SignalMaskHelper mask_helper;
  mask_helper.blockSignal(SIGUSR1);

  // Freeze the cgroup first
  files::WriteFile(freeze_path(test_cgroup_path()), "1");

  pid_t child_pid = fork_helper.RunInForkedProcess([parent_pid] {
    // Set up a signal set to wait for SIGUSR1 which will be sent by the parent process
    test_helper::SignalMaskHelper mask_helper;
    mask_helper.blockSignal(SIGUSR1);

    // Notify the parent that the child starts running.
    mask_helper.waitForSignal(SIGUSR1);
    kill(parent_pid, SIGUSR1);

    // Wait for the SIGUSR1 before adding
    mask_helper.waitForSignal(SIGUSR1);
    kill(parent_pid, SIGUSR1);
  });
  test_pids_.push_back(child_pid);

  // Make sure the child starts running.
  kill(child_pid, SIGUSR1);
  mask_helper.waitForSignal(SIGUSR1);

  // Write the child PID to the cgroup
  files::WriteFile(procs_path(test_cgroup_path()), std::to_string(child_pid));

  // Send signal; frozen child should *not* receive it.
  kill(child_pid, SIGUSR1);

  // Set up a time limit for sigtimedwait. Should timeout without receiving the signal.
  EXPECT_THAT(mask_helper.timedWaitForSignal(SIGUSR1, 1), SyscallFailsWithErrno(EAGAIN));

  // Unfreeze the child process
  files::WriteFile(freeze_path(test_cgroup_path()), "0");

  // Child will process the last signal after thawed.
  mask_helper.waitForSignal(SIGUSR1);

  // Wait for the child process to terminate
  EXPECT_TRUE(fork_helper.WaitForChildren());
}

TEST_F(CgroupFreezerTest, AddProcAfterThawed) {
  pid_t parent_pid = getpid();
  test_helper::ForkHelper fork_helper;

  // Set up a signal set to wait for SIGUSR1 which will be sent by the child process
  test_helper::SignalMaskHelper mask_helper;
  mask_helper.blockSignal(SIGUSR1);

  // Freeze the cgroup first
  files::WriteFile(freeze_path(test_cgroup_path()), "1");

  // Thaw the child process
  files::WriteFile(freeze_path(test_cgroup_path()), "0");

  pid_t child_pid = fork_helper.RunInForkedProcess([parent_pid] {
    // Set up a signal set to wait for SIGUSR1 which will be sent by the parent process
    test_helper::SignalMaskHelper mask_helper;
    mask_helper.blockSignal(SIGUSR1);

    // Wait for the SIGUSR1 before adding
    mask_helper.waitForSignal(SIGUSR1);
    kill(parent_pid, SIGUSR1);
  });
  test_pids_.push_back(child_pid);

  // Write the child PID to the cgroup
  files::WriteFile(procs_path(test_cgroup_path()), std::to_string(child_pid));

  // Send signal; child should receive it.
  kill(child_pid, SIGUSR1);
  mask_helper.waitForSignal(SIGUSR1);

  // Wait for the child process to terminate
  EXPECT_TRUE(fork_helper.WaitForChildren());
}

TEST_F(CgroupFreezerTest, FreezeNestedCgroups) {
  pid_t parent_pid = getpid();
  test_helper::ForkHelper fork_helper;

  // There is no guaranteed order of SIGUSR1 and SIGUSR2 coming from descendant cgroups. Use two
  // signal sets to catch accordingly.
  test_helper::SignalMaskHelper mask_helper_1;
  mask_helper_1.blockSignal(SIGUSR1);
  test_helper::SignalMaskHelper mask_helper_2;
  mask_helper_2.blockSignal(SIGUSR2);

  // Freeze the parent
  files::WriteFile(freeze_path(test_cgroup_path()), "1");

  pid_t child_pid = fork_helper.RunInForkedProcess([parent_pid] {
    // Set up a signal set to wait for SIGUSR1 which will be sent by the parent process
    test_helper::SignalMaskHelper mask_helper;
    mask_helper.blockSignal(SIGUSR1);

    // Wait for the SIGUSR1 that should never be received.
    mask_helper.waitForSignal(SIGUSR1);
    kill(parent_pid, SIGUSR1);
  });
  test_pids_.push_back(child_pid);

  std::string child_cgroup = test_cgroup_path() + "/child";
  create_child_cgroup(child_cgroup);
  ASSERT_TRUE(files::WriteFile(procs_path(child_cgroup), std::to_string(child_pid)));

  pid_t grand_child_pid = fork_helper.RunInForkedProcess([parent_pid] {
    // Set up a signal set to wait for SIGUSR2 which will be sent by the parent process
    test_helper::SignalMaskHelper mask_helper;
    mask_helper.blockSignal(SIGUSR2);

    // Wait for the SIGUSR2 that should never be received.
    mask_helper.waitForSignal(SIGUSR2);
    kill(parent_pid, SIGUSR2);
  });
  test_pids_.push_back(grand_child_pid);

  std::string grand_child_cgroup = child_cgroup + "/grandchild";
  create_child_cgroup(grand_child_cgroup);
  ASSERT_TRUE(files::WriteFile(procs_path(grand_child_cgroup), std::to_string(grand_child_pid)));

  // Send signal; frozen child should *not* receive it.
  kill(child_pid, SIGUSR1);
  EXPECT_THAT(mask_helper_1.timedWaitForSignal(SIGUSR1, 1), SyscallFailsWithErrno(EAGAIN));

  // Send signal; frozen grandchild should *not* receive it.
  kill(grand_child_pid, SIGUSR2);
  EXPECT_THAT(mask_helper_2.timedWaitForSignal(SIGUSR2, 1), SyscallFailsWithErrno(EAGAIN));

  // Keep the child frozen, but thaw the parent
  files::WriteFile(freeze_path(child_cgroup), "1");
  files::WriteFile(freeze_path(test_cgroup_path()), "0");
  EXPECT_THAT(mask_helper_1.timedWaitForSignal(SIGUSR1, 1), SyscallFailsWithErrno(EAGAIN));
  EXPECT_THAT(mask_helper_2.timedWaitForSignal(SIGUSR2, 1), SyscallFailsWithErrno(EAGAIN));

  // Thaw the child cgroup
  files::WriteFile(freeze_path(child_cgroup), "0");

  // Child and grandchild will process the last signal after thawed.
  mask_helper_1.waitForSignal(SIGUSR1);
  mask_helper_2.waitForSignal(SIGUSR2);

  // Wait for the child process to terminate
  EXPECT_TRUE(fork_helper.WaitForChildren());
}

TEST_F(CgroupFreezerTest, CheckStateInEventsFile) {
  // Freeze the cgroup
  files::WriteFile(freeze_path(test_cgroup_path()), "1");
  ASSERT_TRUE(wait_freeze_state_changed(events_path(test_cgroup_path()), true));
}

TEST_F(CgroupFreezerTest, FreezerState) {
  std::string parent_freezer_state;
  std::string child_freezer_state;
  std::string grand_child_freezer_state;
  std::string child_cgroup = test_cgroup_path() + "/child";
  std::string grand_child_cgroup = child_cgroup + "/grandchild";
  create_child_cgroup(child_cgroup);
  create_child_cgroup(grand_child_cgroup);

  // Freeze the parent cgroup
  files::WriteFile(freeze_path(test_cgroup_path()), "1");

  // Frozen parent shouldn't impact the child self state.
  files::ReadFileToString(freeze_path(test_cgroup_path()), &parent_freezer_state);
  EXPECT_EQ(parent_freezer_state, "1\n");
  files::ReadFileToString(freeze_path(child_cgroup), &child_freezer_state);
  EXPECT_EQ(child_freezer_state, "0\n");
  files::ReadFileToString(freeze_path(grand_child_cgroup), &grand_child_freezer_state);
  EXPECT_EQ(grand_child_freezer_state, "0\n");

  // The frozen state in the descendants should be changed.
  ASSERT_TRUE(wait_freeze_state_changed(events_path(test_cgroup_path()), true));
  ASSERT_TRUE(wait_freeze_state_changed(events_path(child_cgroup), true));
  ASSERT_TRUE(wait_freeze_state_changed(events_path(grand_child_cgroup), true));

  // Thaw the child cgroup while the parent is still frozen
  files::WriteFile(freeze_path(child_cgroup), "0");
  ASSERT_TRUE(wait_freeze_state_changed(events_path(child_cgroup), true));
  files::ReadFileToString(freeze_path(child_cgroup), &child_freezer_state);
  EXPECT_EQ(child_freezer_state, "0\n");

  // Thaw the parent cgroup
  files::WriteFile(freeze_path(test_cgroup_path()), "0");

  ASSERT_TRUE(wait_freeze_state_changed(events_path(test_cgroup_path()), false));
  ASSERT_TRUE(wait_freeze_state_changed(events_path(child_cgroup), false));
  ASSERT_TRUE(wait_freeze_state_changed(events_path(grand_child_cgroup), false));

  // Freeze the parent and child cgroups and thaw the parent
  files::WriteFile(freeze_path(test_cgroup_path()), "1");
  files::WriteFile(freeze_path(child_cgroup), "1");
  files::WriteFile(freeze_path(test_cgroup_path()), "0");
  ASSERT_TRUE(wait_freeze_state_changed(events_path(test_cgroup_path()), false));
  ASSERT_TRUE(wait_freeze_state_changed(events_path(child_cgroup), true));
  ASSERT_TRUE(wait_freeze_state_changed(events_path(grand_child_cgroup), true));
  files::ReadFileToString(freeze_path(child_cgroup), &child_freezer_state);
  EXPECT_EQ(child_freezer_state, "1\n");
  files::ReadFileToString(freeze_path(grand_child_cgroup), &grand_child_freezer_state);
  EXPECT_EQ(grand_child_freezer_state, "0\n");
}

TEST_F(CgroupFreezerTest, MoveProcess) {
  // This test verifies that moving a process between frozen and thawed cgroups correctly affects
  // signal delivery.  It creates two frozen child cgroups and starts a child process in one of
  // them.  Signals sent to the child process should be blocked while it's in a frozen cgroup and
  // delivered once it's moved to a thawed cgroup.

  pid_t parent_pid = getpid();
  test_helper::ForkHelper fork_helper;
  test_helper::SignalMaskHelper mask_helper;
  mask_helper.blockSignal(SIGUSR1);

  std::string frozen_child1_cgroup = test_cgroup_path() + "/frozen_child_1";
  create_child_cgroup(frozen_child1_cgroup);
  files::WriteFile(freeze_path(frozen_child1_cgroup), "1");
  std::string frozen_child2_cgroup = test_cgroup_path() + "/frozen_child_2";
  create_child_cgroup(frozen_child2_cgroup);
  files::WriteFile(freeze_path(frozen_child2_cgroup), "1");

  pid_t child_pid = fork_helper.RunInForkedProcess([parent_pid] {
    // Set up a signal set to wait for SIGUSR1 which will be sent by the test(parent) process
    test_helper::SignalMaskHelper mask_helper;
    mask_helper.blockSignal(SIGUSR1);

    mask_helper.waitForSignal(SIGUSR1);
    kill(parent_pid, SIGUSR1);

    mask_helper.waitForSignal(SIGUSR1);
    kill(parent_pid, SIGUSR1);
  });
  test_pids_.push_back(child_pid);

  ASSERT_TRUE(files::WriteFile(procs_path(frozen_child1_cgroup), std::to_string(child_pid)));

  kill(child_pid, SIGUSR1);
  EXPECT_THAT(mask_helper.timedWaitForSignal(SIGUSR1, 1), SyscallFailsWithErrno(EAGAIN));

  // Put the child proc into the root cgroup
  ASSERT_TRUE(files::WriteFile(procs_path(cgroup_path()), std::to_string(child_pid)));
  mask_helper.waitForSignal(SIGUSR1);

  ASSERT_TRUE(files::WriteFile(procs_path(frozen_child2_cgroup), std::to_string(child_pid)));
  kill(child_pid, SIGUSR1);
  EXPECT_THAT(mask_helper.timedWaitForSignal(SIGUSR1, 1), SyscallFailsWithErrno(EAGAIN));

  ASSERT_TRUE(files::WriteFile(procs_path(test_cgroup_path()), std::to_string(child_pid)));
  mask_helper.waitForSignal(SIGUSR1);

  EXPECT_TRUE(fork_helper.WaitForChildren());
}
