// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <grp.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mount.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

#include <fbl/unique_fd.h>
#include <linux/capability.h>

#include "src/starnix/tests/syscalls/cpp/test_helper.h"

namespace {

constexpr uid_t kUser1Uid = 24601;
constexpr gid_t kUser1Gid = kUser1Uid + 1;
constexpr uid_t kUser2Uid = 9430;
constexpr gid_t kUser2Gid = kUser2Uid + 1;

void Become(uid_t user_uid, gid_t group_gid) {
  SAFE_SYSCALL(setgroups(0, nullptr));  // drop all supplementary groups.
  SAFE_SYSCALL(setresgid(group_gid, group_gid, group_gid));
  SAFE_SYSCALL(setresuid(user_uid, user_uid, user_uid));
}

class Poker {
 public:
  explicit Poker(fbl::unique_fd pipe_write_side) : pipe_write_side_(std::move(pipe_write_side)) {}
  Poker(Poker&& o) : pipe_write_side_(std::move(o.pipe_write_side_)) {}
  Poker& operator=(Poker&& o) {
    pipe_write_side_ = std::move(o.pipe_write_side_);
    return *this;
  }
  Poker(const Poker&) = delete;
  Poker& operator=(const Poker&) = delete;

  void poke() { SAFE_SYSCALL(pipe_write_side_.reset()); }

 private:
  fbl::unique_fd pipe_write_side_;
};

class Holder {
 public:
  explicit Holder(fbl::unique_fd pipe_write_side) : pipe_read_side_(std::move(pipe_write_side)) {}
  Holder(Holder&& o) : pipe_read_side_(std::move(o.pipe_read_side_)) {}
  Holder& operator=(Holder&& o) {
    pipe_read_side_ = std::move(o.pipe_read_side_);
    return *this;
  }
  Holder(const Holder&) = delete;
  Holder& operator=(const Holder&) = delete;

  void hold() {
    char never_actually_written;
    SAFE_SYSCALL(read(pipe_read_side_.get(), &never_actually_written, 1));
  }

 private:
  fbl::unique_fd pipe_read_side_;
};

struct Rendezvous {
  Poker poker;
  Holder holder;
};

Rendezvous MakeRendezvous() {
  static_assert(sizeof(int) == sizeof(fbl::unique_fd));
  fbl::unique_fd unique_fds[2];
  SAFE_SYSCALL(pipe(reinterpret_cast<int*>(&unique_fds)));
  return {
      .poker = Poker(std::move(unique_fds[1])),
      .holder = Holder(std::move(unique_fds[0])),
  };
}

testing::AssertionResult CheckPriorityMinimums() {
  errno = 0;
  if (int observed = sched_get_priority_min(-1); observed != -1) {
    return testing::AssertionFailure() << "Expected -1 for an algorithm of -1 but observed "
                                       << observed << " (with errno " << errno << ")";
  }
  if (errno != EINVAL) {
    return testing::AssertionFailure()
           << "Expected EINVAL for errno for an algorithm of -1 but observed " << errno;
  }
  errno = 0;
  if (int observed = sched_get_priority_min(SCHED_OTHER); observed != 0) {
    return testing::AssertionFailure() << "Expected 0 for SCHED_OTHER but observed " << observed
                                       << " (with errno " << errno << ")";
  }
  if (int observed = sched_get_priority_min(SCHED_FIFO); observed != 1) {
    return testing::AssertionFailure() << "Expected 1 for SCHED_FIFO but observed " << observed
                                       << " (with errno " << errno << ")";
  }
  if (int observed = sched_get_priority_min(SCHED_RR); observed != 1) {
    return testing::AssertionFailure() << "Expected 1 for SCHED_RR but observed " << observed
                                       << " (with errno " << errno << ")";
  }
  if (int observed = sched_get_priority_min(SCHED_BATCH); observed != 0) {
    return testing::AssertionFailure() << "Expected 0 for SCHED_BATCH but observed " << observed
                                       << " (with errno " << errno << ")";
  }
  errno = 0;
  if (int observed = sched_get_priority_min(4); observed != -1) {
    return testing::AssertionFailure() << "Expected -1 for an algorithm of 4 but observed "
                                       << observed << " (with errno " << errno << ")";
  }
  if (errno != EINVAL) {
    return testing::AssertionFailure()
           << "Expected EINVAL for errno for an algorithm of 4 but observed " << errno;
  }
  errno = 0;
  if (int observed = sched_get_priority_min(SCHED_IDLE); observed != 0) {
    return testing::AssertionFailure() << "Expected 0 for SCHED_IDLE but observed " << observed
                                       << " (with errno " << errno << ")";
  }
  if (int observed = sched_get_priority_min(SCHED_DEADLINE); observed != 0) {
    return testing::AssertionFailure() << "Expected 0 for SCHED_DEADLINE but observed " << observed
                                       << " (with errno " << errno << ")";
  }
  // Skip to 8 as SCHED_EXT=7 is available in some of the contexts in which
  // we want to run this test.
  errno = 0;
  if (int observed = sched_get_priority_min(8); observed != -1) {
    return testing::AssertionFailure() << "Expected -1 for an algorithm of 8 but observed "
                                       << observed << " (with errno " << errno << ")";
  }
  if (errno != EINVAL) {
    return testing::AssertionFailure()
           << "Expected EINVAL for errno for an algorithm of 8 but observed " << errno;
  }
  return testing::AssertionSuccess();
}

TEST(GetPriorityMinTest, RootCanGetMinimumPriorities) {
  // TODO: https://fxbug.dev/317285180 - drop this after another means is
  // put in place to ensure that this test only ever runs as root.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  EXPECT_TRUE(CheckPriorityMinimums());
}

TEST(GetPriorityMinTest, NonRootCanGetMinimumPriorities) {
  // TODO: https://fxbug.dev/317285180 - drop this after another means is
  // put in place to ensure that this test only ever runs as root.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() {
    Become(kUser1Uid, kUser1Gid);

    EXPECT_TRUE(CheckPriorityMinimums());
  });
}

testing::AssertionResult CheckPriorityMaximums() {
  errno = 0;
  if (int observed = sched_get_priority_max(-1); observed != -1) {
    return testing::AssertionFailure() << "Expected -1 for an algorithm of -1 but observed "
                                       << observed << " (with errno " << errno << ")";
  }
  if (errno != EINVAL) {
    return testing::AssertionFailure()
           << "Expected EINVAL for errno for an algorithm of -1 but observed " << errno;
  }
  errno = 0;
  if (int observed = sched_get_priority_max(SCHED_OTHER); observed != 0) {
    return testing::AssertionFailure() << "Expected 0 for SCHED_OTHER but observed " << observed
                                       << " (with errno " << errno << ")";
  }
  if (int observed = sched_get_priority_max(SCHED_FIFO); observed != 99) {
    return testing::AssertionFailure() << "Expected 99 for SCHED_FIFO but observed " << observed
                                       << " (with errno " << errno << ")";
  }
  if (int observed = sched_get_priority_max(SCHED_RR); observed != 99) {
    return testing::AssertionFailure() << "Expected 99 for SCHED_RR but observed " << observed
                                       << " (with errno " << errno << ")";
  }
  if (int observed = sched_get_priority_max(SCHED_BATCH); observed != 0) {
    return testing::AssertionFailure() << "Expected 0 for SCHED_BATCH but observed " << observed
                                       << " (with errno " << errno << ")";
  }
  errno = 0;
  if (int observed = sched_get_priority_max(4); observed != -1) {
    return testing::AssertionFailure() << "Expected -1 for an algorithm of 4 but observed "
                                       << observed << " (with errno " << errno << ")";
  }
  if (errno != EINVAL) {
    return testing::AssertionFailure()
           << "Expected EINVAL for errno for an algorithm of 4 but observed " << errno;
  }
  errno = 0;
  if (int observed = sched_get_priority_max(SCHED_IDLE); observed != 0) {
    return testing::AssertionFailure() << "Expected 0 for SCHED_IDLE but observed " << observed
                                       << " (with errno " << errno << ")";
  }
  if (int observed = sched_get_priority_max(SCHED_DEADLINE); observed != 0) {
    return testing::AssertionFailure() << "Expected 0 for SCHED_DEADLINE but observed " << observed
                                       << " (with errno " << errno << ")";
  }
  // Skip to 8 as SCHED_EXT=7 is available in some of the contexts in which
  // we want to run this test.
  errno = 0;
  if (int observed = sched_get_priority_max(8); observed != -1) {
    return testing::AssertionFailure() << "Expected -1 for an algorithm of 8 but observed "
                                       << observed << " (with errno " << errno << ")";
  }
  if (errno != EINVAL) {
    return testing::AssertionFailure()
           << "Expected EINVAL for errno for an algorithm of 8 but observed " << errno;
  }
  return testing::AssertionSuccess();
}

TEST(GetPriorityMaxTest, RootCanGetMaximumPriorities) {
  // TODO: https://fxbug.dev/317285180 - drop this after another means is
  // put in place to ensure that this test only ever runs as root.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  EXPECT_TRUE(CheckPriorityMaximums());
}

TEST(GetPriorityMaxTest, NonRootCanGetMaximumPriorities) {
  // TODO: https://fxbug.dev/317285180 - drop this after another means is
  // put in place to ensure that this test only ever runs as root.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() {
    Become(kUser2Uid, kUser2Gid);

    EXPECT_TRUE(CheckPriorityMaximums());
  });
}

TEST(GetPriorityTest, NonRootCanGetNicenessOfUnfriendlyProcess) {
  // TODO: https://fxbug.dev/317285180 - drop this after another means is
  // put in place to ensure that this test only ever runs as root.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper testing_process_fork_helper;
  test_helper::ForkHelper target_process_fork_helper;
  target_process_fork_helper.ExpectSignal(SIGKILL);
  Rendezvous rendezvous = MakeRendezvous();

  pid_t target_pid = target_process_fork_helper.RunInForkedProcess(
      [poker = std::move(rendezvous.poker)]() mutable {
        Become(kUser2Uid, kUser2Gid);

        // After poking the rendezvous to indicate that the desired non-root state
        // has been reached, this process blocks forever.
        poker.poke();
        while (true) {
          pause();
        }
      });

  testing_process_fork_helper.RunInForkedProcess(
      [holder = std::move(rendezvous.holder), target_pid]() mutable {
        Become(kUser1Uid, kUser1Gid);

        holder.hold();

        // We're not testing that the observed niceness is any particular value;
        // we're just testing that the getpriority call succeeds.
        errno = 0;
        getpriority(PRIO_PROCESS, target_pid);
        EXPECT_EQ(errno, 0);
      });
  testing_process_fork_helper.OnlyWaitForForkedChildren();
  EXPECT_TRUE(testing_process_fork_helper.WaitForChildren());

  SAFE_SYSCALL(kill(target_pid, SIGKILL));
}

TEST(GetSchedulerTest, RootCanGetOwnScheduler) {
  // TODO: https://fxbug.dev/317285180 - drop this after another means is
  // put in place to ensure that this test only ever runs as root.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  EXPECT_GE(sched_getscheduler(0), 0);
}

TEST(GetSchedulerTest, RootCanGetSchedulerOfRootOwnedProcess) {
  // TODO: https://fxbug.dev/317285180 - drop this after another means is
  // put in place to ensure that this test only ever runs as root.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper target_process_fork_helper;
  target_process_fork_helper.ExpectSignal(SIGKILL);
  pid_t target_pid = target_process_fork_helper.RunInForkedProcess([]() {
    // This process blocks forever.
    while (true) {
      pause();
    }
  });

  EXPECT_GE(sched_getscheduler(target_pid), 0);

  SAFE_SYSCALL(kill(target_pid, SIGKILL));
}

TEST(GetSchedulerTest, RootCanGetSchedulerOfNonRootOwnedProcess) {
  // TODO: https://fxbug.dev/317285180 - drop this after another means is
  // put in place to ensure that this test only ever runs as root.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper target_process_fork_helper;
  target_process_fork_helper.ExpectSignal(SIGKILL);
  Rendezvous rendezvous = MakeRendezvous();
  pid_t target_pid = target_process_fork_helper.RunInForkedProcess(
      [poker = std::move(rendezvous.poker)]() mutable {
        Become(kUser1Uid, kUser1Gid);

        // After poking the rendezvous to indicate that the desired non-root state
        // has been reached, this process blocks forever.
        poker.poke();
        while (true) {
          pause();
        }
      });

  rendezvous.holder.hold();

  EXPECT_GE(sched_getscheduler(target_pid), 0);

  SAFE_SYSCALL(kill(target_pid, SIGKILL));
}

TEST(GetSchedulerTest, NonRootCanGetOwnScheduler) {
  // TODO: https://fxbug.dev/317285180 - drop this after another means is
  // put in place to ensure that this test only ever runs as root.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper testing_process_fork_helper;
  testing_process_fork_helper.RunInForkedProcess([]() {
    Become(kUser1Uid, kUser1Gid);

    EXPECT_GE(sched_getscheduler(0), 0);
  });
}

TEST(GetSchedulerTest, NonRootCanGetSchedulerOfFriendlyProcess) {
  // TODO: https://fxbug.dev/317285180 - drop this after another means is
  // put in place to ensure that this test only ever runs as root.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper testing_process_fork_helper;
  testing_process_fork_helper.RunInForkedProcess([]() {
    Become(kUser1Uid, kUser1Gid);

    test_helper::ForkHelper target_process_fork_helper;
    target_process_fork_helper.ExpectSignal(SIGKILL);
    pid_t target_pid = target_process_fork_helper.RunInForkedProcess([]() {
      // This process blocks forever.
      while (true) {
        pause();
      }
    });

    EXPECT_GE(sched_getscheduler(target_pid), 0);

    SAFE_SYSCALL(kill(target_pid, SIGKILL));
  });
}

TEST(GetSchedulerTest, NonRootCanGetSchedulerOfUnfriendlyProcess) {
  // TODO: https://fxbug.dev/317285180 - drop this after another means is
  // put in place to ensure that this test only ever runs as root.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper testing_process_fork_helper;
  test_helper::ForkHelper target_process_fork_helper;
  target_process_fork_helper.ExpectSignal(SIGKILL);
  Rendezvous rendezvous = MakeRendezvous();

  pid_t target_pid = target_process_fork_helper.RunInForkedProcess(
      [poker = std::move(rendezvous.poker)]() mutable {
        Become(kUser2Uid, kUser2Gid);

        // After poking the rendezvous to indicate that the desired non-root state
        // has been reached, this process blocks forever.
        poker.poke();
        while (true) {
          pause();
        }
      });

  testing_process_fork_helper.RunInForkedProcess(
      [holder = std::move(rendezvous.holder), target_pid]() mutable {
        Become(kUser1Uid, kUser1Gid);

        holder.hold();

        EXPECT_GE(sched_getscheduler(target_pid), 0);
      });
  testing_process_fork_helper.OnlyWaitForForkedChildren();
  EXPECT_TRUE(testing_process_fork_helper.WaitForChildren());

  SAFE_SYSCALL(kill(target_pid, SIGKILL));
}

testing::AssertionResult SetScheduler(pid_t pid) {
  sched_param param;

  errno = 0;
  param.sched_priority = 0;
  if (int observed = sched_setscheduler(pid, SCHED_OTHER, &param); observed != 0) {
    return testing::AssertionFailure()
           << "Expected to be able to set scheduler of " << pid << " to SCHED_OTHER; failed with "
           << observed << " (and errno " << errno << ")";
  }
  if (int observed_policy = SAFE_SYSCALL(sched_getscheduler(pid)); observed_policy != SCHED_OTHER) {
    return testing::AssertionFailure()
           << "Expected policy of " << pid << " to be SCHED_OTHER; observed policy is "
           << observed_policy;
  }

  int min_fifo_priority = SAFE_SYSCALL(sched_get_priority_min(SCHED_FIFO));
  int max_fifo_priority = SAFE_SYSCALL(sched_get_priority_max(SCHED_FIFO));
  for (auto priority = min_fifo_priority; priority <= max_fifo_priority; priority++) {
    param.sched_priority = priority;
    if (int observed = sched_setscheduler(pid, SCHED_FIFO, &param); observed != 0) {
      return testing::AssertionFailure()
             << "Expected to be able to set scheduler of " << pid << " to SCHED_FIFO with priority "
             << priority << "; failed with " << observed << " (and errno " << errno << ")";
    }
    if (int observed_policy = SAFE_SYSCALL(sched_getscheduler(pid));
        observed_policy != SCHED_FIFO) {
      return testing::AssertionFailure()
             << "Expected policy of " << pid << " to be SCHED_FIFO; observed policy is "
             << observed_policy;
    }
    sched_param observed_param = {.sched_priority = -1};
    SAFE_SYSCALL(sched_getparam(pid, &observed_param));
    if (observed_param.sched_priority != priority) {
      return testing::AssertionFailure()
             << "Expected priority of " << pid << " to be " << priority << "; observed priority is "
             << observed_param.sched_priority;
    }
  }

  int min_rr_priority = SAFE_SYSCALL(sched_get_priority_min(SCHED_RR));
  int max_rr_priority = SAFE_SYSCALL(sched_get_priority_max(SCHED_RR));
  for (auto priority = min_rr_priority; priority <= max_rr_priority; priority++) {
    param.sched_priority = priority;
    if (int observed = sched_setscheduler(pid, SCHED_RR, &param); observed != 0) {
      return testing::AssertionFailure()
             << "Expected to be able to set scheduler of " << pid << " to SCHED_RR with priority "
             << priority << "; failed with " << observed << " (and errno " << errno << ")";
    }
    if (int observed_policy = SAFE_SYSCALL(sched_getscheduler(pid)); observed_policy != SCHED_RR) {
      return testing::AssertionFailure()
             << "Expected policy of " << pid << " to be SCHED_RR; observed policy is "
             << observed_policy;
    }
    sched_param observed_param = {.sched_priority = -1};
    SAFE_SYSCALL(sched_getparam(pid, &observed_param));
    if (observed_param.sched_priority != priority) {
      return testing::AssertionFailure()
             << "Expected priority of " << pid << " to be " << priority << "; observed priority is "
             << observed_param.sched_priority;
    }
  }

  errno = 0;
  param.sched_priority = 0;
  if (int observed = sched_setscheduler(pid, SCHED_BATCH, &param); observed != 0) {
    return testing::AssertionFailure()
           << "Expected to be able to set scheduler of " << pid << " to SCHED_BATCH; failed with "
           << observed << " (and errno " << errno << ")";
  }
  if (int observed_policy = SAFE_SYSCALL(sched_getscheduler(pid)); observed_policy != SCHED_BATCH) {
    return testing::AssertionFailure()
           << "Expected policy of " << pid << " to be SCHED_BATCH; observed policy is "
           << observed_policy;
  }

  errno = 0;
  param.sched_priority = 0;
  if (int observed = sched_setscheduler(pid, SCHED_IDLE, &param); observed != 0) {
    return testing::AssertionFailure()
           << "Expected to be able to set scheduler of " << pid << " to SCHED_IDLE; failed with "
           << observed << " (and errno " << errno << ")";
  }
  if (int observed_policy = SAFE_SYSCALL(sched_getscheduler(pid)); observed_policy != SCHED_IDLE) {
    return testing::AssertionFailure()
           << "Expected policy of " << pid << " to be SCHED_IDLE; observed policy is "
           << observed_policy;
  }

  return testing::AssertionSuccess();
}

TEST(SetSchedulerTest, InvalidArguments) {
  sched_param param;

  // See "Invalid arguments: pid is negative or param is NULL" at
  // sched_setscheduler(2).
  errno = 0;
  EXPECT_EQ(sched_setscheduler(-1, SCHED_RR, &param), -1);
  EXPECT_EQ(errno, EINVAL);
  errno = 0;
  EXPECT_EQ(sched_setscheduler(-7, SCHED_RR, &param), -1);
  EXPECT_EQ(errno, EINVAL);
  errno = 0;
  EXPECT_EQ(sched_setscheduler(0, SCHED_RR, nullptr), -1);
  EXPECT_EQ(errno, EINVAL);

  // See "policy is not one of the recognized policies" at sched_setscheduler(7).
  errno = 0;
  EXPECT_EQ(sched_setscheduler(0, 4, nullptr), -1);
  EXPECT_EQ(errno, EINVAL);

  // Real-time priority differs from niceness in that attempting to set a value
  // outside the system-supported range will be rejected rather than clamped.
  //
  // (This is also "param does not make sense for the specified policy" at
  // sched_setscheduler(7).)
  errno = 0;
  param.sched_priority = SAFE_SYSCALL(sched_get_priority_min(SCHED_RR)) - 20;
  EXPECT_EQ(sched_setscheduler(0, SCHED_RR, &param), -1);
  EXPECT_EQ(errno, EINVAL);
  errno = 0;
  param.sched_priority = SAFE_SYSCALL(sched_get_priority_min(SCHED_RR)) - 1;
  EXPECT_EQ(sched_setscheduler(0, SCHED_RR, &param), -1);
  EXPECT_EQ(errno, EINVAL);
  errno = 0;
  param.sched_priority = SAFE_SYSCALL(sched_get_priority_max(SCHED_RR)) + 1;
  EXPECT_EQ(sched_setscheduler(0, SCHED_RR, &param), -1);
  EXPECT_EQ(errno, EINVAL);
  errno = 0;
  param.sched_priority = SAFE_SYSCALL(sched_get_priority_max(SCHED_RR)) + 20;
  EXPECT_EQ(sched_setscheduler(0, SCHED_RR, &param), -1);
  EXPECT_EQ(errno, EINVAL);

  // For non-real-time policies, the priority must be zero. (See "For each of the
  // above policies, param->sched_priority must be 0" at sched_setscheduler(7).)
  errno = 0;
  param.sched_priority = -1;
  EXPECT_EQ(sched_setscheduler(0, SCHED_OTHER, &param), -1);
  EXPECT_EQ(errno, EINVAL);
  errno = 0;
  param.sched_priority = 1;
  EXPECT_EQ(sched_setscheduler(0, SCHED_OTHER, &param), -1);
  EXPECT_EQ(errno, EINVAL);
  errno = 0;
  param.sched_priority = -1;
  EXPECT_EQ(sched_setscheduler(0, SCHED_BATCH, &param), -1);
  EXPECT_EQ(errno, EINVAL);
  errno = 0;
  param.sched_priority = 1;
  EXPECT_EQ(sched_setscheduler(0, SCHED_BATCH, &param), -1);
  EXPECT_EQ(errno, EINVAL);
  errno = 0;
  param.sched_priority = -1;
  EXPECT_EQ(sched_setscheduler(0, SCHED_IDLE, &param), -1);
  EXPECT_EQ(errno, EINVAL);
  errno = 0;
  param.sched_priority = 1;
  EXPECT_EQ(sched_setscheduler(0, SCHED_IDLE, &param), -1);
  EXPECT_EQ(errno, EINVAL);

  // SCHED_DEADLINE is not usable with sched_setscheduler; see "To set and fetch
  // this policy and associated attributes, one must use the Linux-specific
  // sched_setattr(2) and sched_getattr(2) system calls" at sched(7).
  errno = 0;
  param.sched_priority = 0;
  EXPECT_EQ(sched_setscheduler(0, SCHED_DEADLINE, &param), -1);
  EXPECT_EQ(errno, EINVAL);
}

TEST(SetSchedulerTest, RootCanSetOwnScheduler) {
  // TODO: https://fxbug.dev/317285180 - drop this after another means is
  // put in place to ensure that this test only ever runs as root.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() { EXPECT_TRUE(SetScheduler(0)); });
}

TEST(SetSchedulerTest, RootCanSetSchedulerOfRootOwnedProcess) {
  // TODO: https://fxbug.dev/317285180 - drop this after another means is
  // put in place to ensure that this test only ever runs as root.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() {
    test_helper::ForkHelper target_process_fork_helper;
    target_process_fork_helper.ExpectSignal(SIGKILL);
    pid_t target_pid = target_process_fork_helper.RunInForkedProcess([]() {
      // This process blocks forever.
      while (true) {
        pause();
      }
    });

    EXPECT_TRUE(SetScheduler(target_pid));

    SAFE_SYSCALL(kill(target_pid, SIGKILL));
  });
}

TEST(SetSchedulerTest, RootCanSetSchedulerOfNonRootOwnedProcess) {
  // TODO: https://fxbug.dev/317285180 - drop this after another means is
  // put in place to ensure that this test only ever runs as root.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() mutable {
    Rendezvous rendezvous = MakeRendezvous();
    test_helper::ForkHelper target_process_fork_helper;
    target_process_fork_helper.ExpectSignal(SIGKILL);
    pid_t target_pid = target_process_fork_helper.RunInForkedProcess(
        [poker = std::move(rendezvous.poker)]() mutable {
          Become(kUser1Uid, kUser1Gid);

          // After poking the rendezvous to indicate that the desired non-root state
          // has been reached, this process blocks forever.
          poker.poke();
          while (true) {
            pause();
          }
        });

    rendezvous.holder.hold();

    EXPECT_TRUE(SetScheduler(target_pid));

    SAFE_SYSCALL(kill(target_pid, SIGKILL));
  });
}

TEST(SetSchedulerTest, NonRootCanSetOwnScheduler) {
  // TODO: https://fxbug.dev/317285180 - drop this after another means is
  // put in place to ensure that this test only ever runs as root.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() {
    // Remove rlimits from consideration in this test.
    struct rlimit limit = {
        .rlim_cur = RLIM_INFINITY,
        .rlim_max = RLIM_INFINITY,
    };
    SAFE_SYSCALL(setrlimit(RLIMIT_RTPRIO, &limit));

    Become(kUser1Uid, kUser1Gid);

    EXPECT_TRUE(SetScheduler(0));
  });
}

TEST(SetSchedulerTest, NonRootCanSetSchedulerOfFriendlyProcess) {
  // TODO: https://fxbug.dev/317285180 - drop this after another means is
  // put in place to ensure that this test only ever runs as root.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper testing_process_fork_helper;
  test_helper::ForkHelper target_process_fork_helper;
  Rendezvous rendezvous = MakeRendezvous();
  target_process_fork_helper.ExpectSignal(SIGKILL);

  pid_t target_pid = target_process_fork_helper.RunInForkedProcess(
      [poker = std::move(rendezvous.poker)]() mutable {
        // Remove rlimits from consideration in this test.
        struct rlimit limit = {
            .rlim_cur = RLIM_INFINITY,
            .rlim_max = RLIM_INFINITY,
        };
        SAFE_SYSCALL(setrlimit(RLIMIT_RTPRIO, &limit));

        Become(kUser1Uid, kUser1Gid);

        // After poking the rendezvous to indicate that the desired non-root state
        // has been reached, this process blocks forever.
        poker.poke();
        while (true) {
          pause();
        }
      });

  testing_process_fork_helper.RunInForkedProcess(
      [holder = std::move(rendezvous.holder), target_pid]() mutable {
        Become(kUser1Uid, kUser1Gid);

        holder.hold();

        EXPECT_TRUE(SetScheduler(target_pid));
      });
  testing_process_fork_helper.OnlyWaitForForkedChildren();
  EXPECT_TRUE(testing_process_fork_helper.WaitForChildren());

  SAFE_SYSCALL(kill(target_pid, SIGKILL));
}

TEST(SetSchedulerTest, ResetOnForkShiftsToNonRealTimePolicy) {
  // TODO: https://fxbug.dev/317285180 - drop this after another means is
  // put in place to ensure that this test only ever runs as root.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() {
    sched_param param = {.sched_priority = 2};
    EXPECT_EQ(sched_setscheduler(0, SCHED_FIFO | SCHED_RESET_ON_FORK, &param), 0);

    Become(kUser1Uid, kUser1Gid);

    test_helper::ForkHelper target_process_fork_helper;
    target_process_fork_helper.ExpectSignal(SIGKILL);
    pid_t target_pid = target_process_fork_helper.RunInForkedProcess([]() {
      // This process blocks forever.
      while (true) {
        pause();
      }
    });

    EXPECT_EQ(sched_getscheduler(target_pid), SCHED_OTHER);

    SAFE_SYSCALL(kill(target_pid, SIGKILL));
  });
}

TEST(SetSchedulerTest, ResetOnForkShiftsToNonRealTimePolicyEvenForRoot) {
  // TODO: https://fxbug.dev/317285180 - drop this after another means is
  // put in place to ensure that this test only ever runs as root.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() {
    sched_param param = {.sched_priority = 2};
    EXPECT_EQ(sched_setscheduler(0, SCHED_FIFO | SCHED_RESET_ON_FORK, &param), 0);

    // No transition to an ordinary user here; we remain root!

    test_helper::ForkHelper target_process_fork_helper;
    target_process_fork_helper.ExpectSignal(SIGKILL);
    pid_t target_pid = target_process_fork_helper.RunInForkedProcess([]() {
      // This process blocks forever.
      while (true) {
        pause();
      }
    });

    EXPECT_EQ(sched_getscheduler(target_pid), SCHED_OTHER);

    SAFE_SYSCALL(kill(target_pid, SIGKILL));
  });
}

TEST(SetSchedulerTest, ResetOnForkZeroesNegativeNiceness) {
  // TODO: https://fxbug.dev/317285180 - drop this after another means is
  // put in place to ensure that this test only ever runs as root.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() {
    sched_param param = {.sched_priority = 0};
    EXPECT_EQ(sched_setscheduler(0, SCHED_BATCH | SCHED_RESET_ON_FORK, &param), 0);
    EXPECT_EQ(setpriority(PRIO_PROCESS, 0, -10), 0);

    Become(kUser1Uid, kUser1Gid);

    test_helper::ForkHelper target_process_fork_helper;
    target_process_fork_helper.ExpectSignal(SIGKILL);
    pid_t target_pid = target_process_fork_helper.RunInForkedProcess([]() {
      // This process blocks forever.
      while (true) {
        pause();
      }
    });

    EXPECT_EQ(sched_getscheduler(target_pid), SCHED_BATCH);
    EXPECT_EQ(getpriority(PRIO_PROCESS, target_pid), 0);

    SAFE_SYSCALL(kill(target_pid, SIGKILL));
  });
}

TEST(GetParamTest, RootCanGetOwnParam) {
  // TODO: https://fxbug.dev/317285180 - drop this after another means is
  // put in place to ensure that this test only ever runs as root.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  sched_param observed_param;
  observed_param.sched_priority = -1;

  EXPECT_EQ(sched_getparam(0, &observed_param), 0);
  EXPECT_GE(observed_param.sched_priority, 0);
}

TEST(GetParamTest, RootCanGetParamOfRootOwnedProcess) {
  // TODO: https://fxbug.dev/317285180 - drop this after another means is
  // put in place to ensure that this test only ever runs as root.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper target_process_fork_helper;
  target_process_fork_helper.ExpectSignal(SIGKILL);
  pid_t target_pid = target_process_fork_helper.RunInForkedProcess([]() {
    // This process blocks forever.
    while (true) {
      pause();
    }
  });

  sched_param observed_param;
  observed_param.sched_priority = -1;

  EXPECT_EQ(sched_getparam(target_pid, &observed_param), 0);
  EXPECT_GE(observed_param.sched_priority, 0);

  SAFE_SYSCALL(kill(target_pid, SIGKILL));
}

TEST(GetParamTest, RootCanGetParamOfNonRootOwnedProcess) {
  // TODO: https://fxbug.dev/317285180 - drop this after another means is
  // put in place to ensure that this test only ever runs as root.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper target_process_fork_helper;
  Rendezvous rendezvous = MakeRendezvous();
  target_process_fork_helper.ExpectSignal(SIGKILL);

  pid_t target_pid = target_process_fork_helper.RunInForkedProcess(
      [poker = std::move(rendezvous.poker)]() mutable {
        Become(kUser1Uid, kUser1Gid);

        // After poking the rendezvous to indicate that the desired non-root state
        // has been reached, this process blocks forever.
        poker.poke();
        while (true) {
          pause();
        }
      });

  rendezvous.holder.hold();

  sched_param observed_param;
  observed_param.sched_priority = -1;

  EXPECT_EQ(sched_getparam(target_pid, &observed_param), 0);
  EXPECT_GE(observed_param.sched_priority, 0);

  SAFE_SYSCALL(kill(target_pid, SIGKILL));
}

TEST(GetParamTest, NonRootCanGetOwnScheduler) {
  // TODO: https://fxbug.dev/317285180 - drop this after another means is
  // put in place to ensure that this test only ever runs as root.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper testing_process_fork_helper;
  testing_process_fork_helper.RunInForkedProcess([]() {
    Become(kUser1Uid, kUser1Gid);

    sched_param observed_param;
    observed_param.sched_priority = -1;

    EXPECT_EQ(sched_getparam(0, &observed_param), 0);
    EXPECT_GE(observed_param.sched_priority, 0);
  });
}

TEST(GetParamTest, NonRootCanGetParamOfFriendlyProcess) {
  // TODO: https://fxbug.dev/317285180 - drop this after another means is
  // put in place to ensure that this test only ever runs as root.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper testing_process_fork_helper;
  testing_process_fork_helper.RunInForkedProcess([]() {
    Become(kUser1Uid, kUser1Gid);

    test_helper::ForkHelper target_process_fork_helper;
    target_process_fork_helper.ExpectSignal(SIGKILL);
    pid_t target_pid = target_process_fork_helper.RunInForkedProcess([]() {
      // This process blocks forever.
      while (true) {
        pause();
      }
    });

    sched_param observed_param;
    observed_param.sched_priority = -1;

    EXPECT_EQ(sched_getparam(target_pid, &observed_param), 0);
    EXPECT_GE(observed_param.sched_priority, 0);

    SAFE_SYSCALL(kill(target_pid, SIGKILL));
  });
}

TEST(GetParamTest, NonRootCanGetParamOfUnfriendlyProcess) {
  // TODO: https://fxbug.dev/317285180 - drop this after another means is
  // put in place to ensure that this test only ever runs as root.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper testing_process_fork_helper;
  test_helper::ForkHelper target_process_fork_helper;
  Rendezvous rendezvous = MakeRendezvous();
  target_process_fork_helper.ExpectSignal(SIGKILL);

  pid_t target_pid = target_process_fork_helper.RunInForkedProcess(
      [poker = std::move(rendezvous.poker)]() mutable {
        Become(kUser2Uid, kUser2Gid);

        // After poking the rendezvous to indicate that the desired non-root state
        // has been reached, this process blocks forever.
        poker.poke();
        while (true) {
          pause();
        }
      });

  testing_process_fork_helper.RunInForkedProcess(
      [holder = std::move(rendezvous.holder), target_pid]() mutable {
        Become(kUser1Uid, kUser1Gid);

        holder.hold();

        sched_param observed_param;
        observed_param.sched_priority = -1;

        EXPECT_EQ(sched_getparam(target_pid, &observed_param), 0);
        EXPECT_GE(observed_param.sched_priority, 0);
      });
  testing_process_fork_helper.OnlyWaitForForkedChildren();
  EXPECT_TRUE(testing_process_fork_helper.WaitForChildren());

  SAFE_SYSCALL(kill(target_pid, SIGKILL));
}

testing::AssertionResult SetParam(pid_t pid) {
  sched_param param;
  sched_param observed_param;

  param.sched_priority = 0;
  SAFE_SYSCALL(sched_setscheduler(pid, SCHED_OTHER, &param));
  param.sched_priority = 0;
  if (int observed = sched_setparam(pid, &param); observed != 0) {
    return testing::AssertionFailure()
           << "Expected to be able to perform the nullipotent operation of setting to 0 the priority of "
           << pid
           << " which already has priority 0 (and is of a policy that doesn't use priority anyway!)";
  }
  observed_param.sched_priority = -1;
  SAFE_SYSCALL(sched_getparam(pid, &observed_param));
  if (observed_param.sched_priority != 0) {
    return testing::AssertionFailure()
           << "Expected that after successfully setting priority of " << pid
           << " to 0, retrieving the priority would also show it to be 0, but observed "
           << observed_param.sched_priority;
  }

  int min_fifo_priority = SAFE_SYSCALL(sched_get_priority_min(SCHED_FIFO));
  int max_fifo_priority = SAFE_SYSCALL(sched_get_priority_max(SCHED_FIFO));
  param.sched_priority = min_fifo_priority;
  SAFE_SYSCALL(sched_setscheduler(pid, SCHED_FIFO, &param));
  for (auto priority = min_fifo_priority; priority <= max_fifo_priority; priority++) {
    param.sched_priority = priority;
    if (int observed = sched_setparam(pid, &param); observed != 0) {
      return testing::AssertionFailure() << "Expected to be able to set the priority of " << pid
                                         << " to " << priority << " but failed with " << observed;
    }
    observed_param.sched_priority = -1;
    SAFE_SYSCALL(sched_getparam(pid, &observed_param));
    if (observed_param.sched_priority != priority) {
      return testing::AssertionFailure()
             << "Expected that after successfully setting priority of " << pid << " to " << priority
             << ", retrieving the priority would also show it to be " << priority
             << ", but observed " << observed_param.sched_priority;
    }
  }

  int min_rr_priority = SAFE_SYSCALL(sched_get_priority_min(SCHED_RR));
  int max_rr_priority = SAFE_SYSCALL(sched_get_priority_max(SCHED_RR));
  param.sched_priority = min_rr_priority;
  SAFE_SYSCALL(sched_setscheduler(pid, SCHED_RR, &param));
  for (auto priority = min_rr_priority; priority <= max_rr_priority; priority++) {
    param.sched_priority = priority;
    if (int observed = sched_setparam(pid, &param); observed != 0) {
      return testing::AssertionFailure() << "Expected to be able to set the priority of " << pid
                                         << " to " << priority << " but failed with " << observed;
    }
    observed_param.sched_priority = -1;
    SAFE_SYSCALL(sched_getparam(pid, &observed_param));
    if (observed_param.sched_priority != priority) {
      return testing::AssertionFailure()
             << "Expected that after successfully setting priority of " << pid << " to " << priority
             << ", retrieving the priority would also show it to be " << priority
             << ", but observed " << observed_param.sched_priority;
    }
  }

  param.sched_priority = 0;
  SAFE_SYSCALL(sched_setscheduler(pid, SCHED_BATCH, &param));
  param.sched_priority = 0;
  if (int observed = sched_setparam(pid, &param); observed != 0) {
    return testing::AssertionFailure()
           << "Expected to be able to perform the nullipotent operation of setting to 0 the priority of "
           << pid
           << " which already has priority 0 (and is of a policy that doesn't use priority anyway!)";
  }
  observed_param.sched_priority = -1;
  SAFE_SYSCALL(sched_getparam(pid, &observed_param));
  if (observed_param.sched_priority != 0) {
    return testing::AssertionFailure()
           << "Expected that after successfully setting priority of " << pid
           << " to 0, retrieving the priority would also show it to be 0, but observed "
           << observed_param.sched_priority;
  }

  param.sched_priority = 0;
  SAFE_SYSCALL(sched_setscheduler(pid, SCHED_IDLE, &param));
  param.sched_priority = 0;
  if (int observed = sched_setparam(pid, &param); observed != 0) {
    return testing::AssertionFailure()
           << "Expected to be able to perform the nullipotent operation of setting to 0 the priority of "
           << pid
           << " which already has priority 0 (and is of a policy that doesn't use priority anyway!)";
  }
  observed_param.sched_priority = -1;
  SAFE_SYSCALL(sched_getparam(pid, &observed_param));
  if (observed_param.sched_priority != 0) {
    return testing::AssertionFailure()
           << "Expected that after successfully setting priority of " << pid
           << " to 0, retrieving the priority would also show it to be 0, but observed "
           << observed_param.sched_priority;
  }

  return testing::AssertionSuccess();
}

TEST(SetParamTest, InvalidArguments) {
  // TODO: https://fxbug.dev/317285180 - drop this after another means is
  // put in place to ensure that this test only ever runs as root.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  sched_param param;

  // See "Invalid arguments: param is NULL or pid is negative" at
  // sched_setparam(2).
  errno = 0;
  EXPECT_EQ(sched_setparam(-1, &param), -1);
  EXPECT_EQ(errno, EINVAL);
  errno = 0;
  EXPECT_EQ(sched_setparam(-7, &param), -1);
  EXPECT_EQ(errno, EINVAL);
  errno = 0;
  EXPECT_EQ(sched_setparam(0, nullptr), -1);
  EXPECT_EQ(errno, EINVAL);

  // See "The argument param does not make sense for the current
  // scheduling policy" at sched_setparam(2).
  int min_rr_priority = SAFE_SYSCALL(sched_get_priority_min(SCHED_RR));
  param.sched_priority = min_rr_priority;
  SAFE_SYSCALL(sched_setscheduler(0, SCHED_RR, &param));
  errno = 0;
  param.sched_priority = min_rr_priority - 20;
  EXPECT_EQ(sched_setparam(0, &param), -1);
  EXPECT_EQ(errno, EINVAL);
}

TEST(SetParamTest, RootCanSetOwnScheduler) {
  // TODO: https://fxbug.dev/317285180 - drop this after another means is
  // put in place to ensure that this test only ever runs as root.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() { EXPECT_TRUE(SetParam(0)); });
}

TEST(SetParamTest, RootCanSetParamOfRootOwnedProcess) {
  // TODO: https://fxbug.dev/317285180 - drop this after another means is
  // put in place to ensure that this test only ever runs as root.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() {
    test_helper::ForkHelper target_process_fork_helper;
    target_process_fork_helper.ExpectSignal(SIGKILL);
    pid_t target_pid = target_process_fork_helper.RunInForkedProcess([]() {
      // This process blocks forever.
      while (true) {
        pause();
      }
    });

    EXPECT_TRUE(SetParam(target_pid));

    SAFE_SYSCALL(kill(target_pid, SIGKILL));
  });
}

TEST(SetParamTest, RootCanSetParamOfNonRootOwnedProcess) {
  // TODO: https://fxbug.dev/317285180 - drop this after another means is
  // put in place to ensure that this test only ever runs as root.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() {
    Rendezvous rendezvous = MakeRendezvous();
    test_helper::ForkHelper target_process_fork_helper;
    target_process_fork_helper.ExpectSignal(SIGKILL);

    pid_t target_pid = target_process_fork_helper.RunInForkedProcess(
        [poker = std::move(rendezvous.poker)]() mutable {
          Become(kUser1Uid, kUser1Gid);

          // After poking the rendezvous to indicate that the desired non-root state
          // has been reached, this process blocks forever.
          poker.poke();
          while (true) {
            pause();
          }
        });

    rendezvous.holder.hold();

    EXPECT_TRUE(SetParam(target_pid));

    SAFE_SYSCALL(kill(target_pid, SIGKILL));
  });
}

TEST(SetParamTest, NonRootCanSetOwnParam) {
  // TODO: https://fxbug.dev/317285180 - drop this after another means is
  // put in place to ensure that this test only ever runs as root.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() {
    // Remove rlimits from consideration in this test.
    struct rlimit limit = {
        .rlim_cur = RLIM_INFINITY,
        .rlim_max = RLIM_INFINITY,
    };
    SAFE_SYSCALL(setrlimit(RLIMIT_RTPRIO, &limit));

    Become(kUser1Uid, kUser1Gid);

    EXPECT_TRUE(SetParam(0));
  });
}

TEST(SetParamTest, NonRootCanSetParamOfFriendlyProcess) {
  // TODO: https://fxbug.dev/317285180 - drop this after another means is
  // put in place to ensure that this test only ever runs as root.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper testing_process_fork_helper;
  test_helper::ForkHelper target_process_fork_helper;
  Rendezvous rendezvous = MakeRendezvous();
  target_process_fork_helper.ExpectSignal(SIGKILL);

  pid_t target_pid = target_process_fork_helper.RunInForkedProcess(
      [poker = std::move(rendezvous.poker)]() mutable {
        // Remove rlimits from consideration in this test.
        struct rlimit limit = {
            .rlim_cur = RLIM_INFINITY,
            .rlim_max = RLIM_INFINITY,
        };
        SAFE_SYSCALL(setrlimit(RLIMIT_RTPRIO, &limit));

        Become(kUser1Uid, kUser1Gid);

        // After poking the rendezvous to indicate that the desired non-root state
        // has been reached, this process blocks forever.
        poker.poke();
        while (true) {
          pause();
        }
      });

  testing_process_fork_helper.RunInForkedProcess(
      [holder = std::move(rendezvous.holder), target_pid]() mutable {
        Become(kUser1Uid, kUser1Gid);

        holder.hold();

        EXPECT_TRUE(SetParam(target_pid));
      });
  testing_process_fork_helper.OnlyWaitForForkedChildren();
  EXPECT_TRUE(testing_process_fork_helper.WaitForChildren());

  SAFE_SYSCALL(kill(target_pid, SIGKILL));
}

}  // namespace
