// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// NOTE(nathaniel): this test is not supported to be run as not-root. We
// might choose in the future to adapt it to give some valid results when
// run as not-root, but that's not decided today.
//
// TODO: https://fxbug.dev/317285180 - drop the many GTEST_SKIPs in this
// file after another mechanism is put in place ensuring that this test
// only ever runs as root (or, if the day ever comes, support for being run
// as non-root is added).

#include <fcntl.h>
#include <grp.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mount.h>
#include <sys/resource.h>
#include <sys/select.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

#include <fbl/unique_fd.h>
#include <linux/capability.h>

#include "src/starnix/tests/syscalls/cpp/test_helper.h"

namespace {

constexpr uid_t kUser1Uid = 24601;
constexpr uid_t kUser2Uid = 9430;
constexpr gid_t kGid = kUser1Uid + kUser2Uid;

void Become(uid_t user_uid, uid_t effective_user_uid) {
  SAFE_SYSCALL(setgroups(0, nullptr));  // drop all supplementary groups.
  SAFE_SYSCALL(setresgid(kGid, kGid, kGid));
  SAFE_SYSCALL(setresuid(user_uid, effective_user_uid, user_uid));
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

  void poke() {
    char clog[] = "clog";  // This data will enter but never leave the pipe.
    SAFE_SYSCALL(write(pipe_write_side_.get(), clog, sizeof(clog)));
    SAFE_SYSCALL(pipe_write_side_.reset());
  }

 private:
  fbl::unique_fd pipe_write_side_;
};

class Holder {
 public:
  explicit Holder(fbl::unique_fd pipe_read_side) : pipe_read_side_(std::move(pipe_read_side)) {}
  Holder(Holder&& o) : pipe_read_side_(std::move(o.pipe_read_side_)) {}
  Holder& operator=(Holder&& o) {
    pipe_read_side_ = std::move(o.pipe_read_side_);
    return *this;
  }
  Holder(const Holder&) = delete;
  Holder& operator=(const Holder&) = delete;

  void hold() {
    int pipe_read_side_fd = pipe_read_side_.get();
    fd_set read_fds;
    FD_ZERO(&read_fds);
    FD_SET(pipe_read_side_fd, &read_fds);
    SAFE_SYSCALL(select(pipe_read_side_fd + 1, &read_fds, nullptr, nullptr, nullptr));
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

pid_t SpawnTarget(test_helper::ForkHelper& fork_helper, Poker ready, Holder complete,
                  fit::function<void()> prepare) {
  return fork_helper.RunInForkedProcess([ready = std::move(ready), complete = std::move(complete),
                                         prepare = std::move(prepare)]() mutable {
    prepare();
    ready.poke();
    complete.hold();
  });
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

TEST(SchedGetPriorityMinTest, RootCanGetMinimumPriorities) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  EXPECT_TRUE(CheckPriorityMinimums());
}

TEST(SchedGetPriorityMinTest, NonRootCanGetMinimumPriorities) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() {
    Become(kUser1Uid, kUser1Uid);

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

TEST(SchedGetPriorityMaxTest, RootCanGetMaximumPriorities) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  EXPECT_TRUE(CheckPriorityMaximums());
}

TEST(SchedGetPriorityMaxTest, NonRootCanGetMaximumPriorities) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() {
    Become(kUser2Uid, kUser2Uid);

    EXPECT_TRUE(CheckPriorityMaximums());
  });
}

TEST(GetPriorityTest, NonRootCanGetNicenessOfUnfriendlyProcess) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper fork_helper;
  Rendezvous ready = MakeRendezvous();
  Rendezvous complete = MakeRendezvous();

  pid_t target_pid = SpawnTarget(fork_helper, std::move(ready.poker), std::move(complete.holder),
                                 []() { Become(kUser2Uid, kUser2Uid); });

  fork_helper.RunInForkedProcess([ready = std::move(ready.holder),
                                  complete = std::move(complete.poker), target_pid]() mutable {
    Become(kUser1Uid, kUser1Uid);

    ready.hold();

    // We're not testing that the observed niceness is any particular value;
    // we're just testing that the getpriority call succeeds.
    errno = 0;
    getpriority(PRIO_PROCESS, target_pid);
    EXPECT_EQ(errno, 0);

    complete.poke();
  });
}

TEST(SetPriorityTest, NonRootCanSetNicenessOfFriendlyProcess) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper fork_helper;
  Rendezvous ready = MakeRendezvous();
  Rendezvous complete = MakeRendezvous();

  pid_t target_pid =
      SpawnTarget(fork_helper, std::move(ready.poker), std::move(complete.holder), []() {
        // Remove rlimits from consideration in this test.
        struct rlimit limit = {
            .rlim_cur = RLIM_INFINITY,
            .rlim_max = RLIM_INFINITY,
        };
        SAFE_SYSCALL(setrlimit(RLIMIT_NICE, &limit));

        // A target process with a different UID but an effective UID that
        // matches the effective UID of the effective UID of the
        // calling-the-setpriority-syscall process counts as "friendly" and
        // the syscall will succeed.
        //
        // See "Linux 2.6.12 and later require the effective user ID of
        // the caller to match the real or effective user ID of the process
        // who" at setpriority(2).
        Become(kUser2Uid, kUser1Uid);
      });

  fork_helper.RunInForkedProcess([ready = std::move(ready.holder),
                                  complete = std::move(complete.poker), target_pid]() mutable {
    Become(kUser1Uid, kUser1Uid);

    ready.hold();

    for (int niceness = 19; niceness >= -20; niceness--) {
      EXPECT_EQ(setpriority(PRIO_PROCESS, target_pid, niceness), 0);
      errno = 0;
      EXPECT_EQ(getpriority(PRIO_PROCESS, target_pid), niceness);
      EXPECT_EQ(errno, 0);
    }

    complete.poke();
  });
}

TEST(SetPriorityTest, OutOfRangeNicenessValuesGetClamped) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() {
    int too_nice = 88;            // Higher than 19.
    int not_nice_enough = -1337;  // Lower than -20.

    EXPECT_EQ(setpriority(PRIO_PROCESS, 0, too_nice), 0);
    EXPECT_EQ(getpriority(PRIO_PROCESS, 0), 19);

    EXPECT_EQ(setpriority(PRIO_PROCESS, 0, not_nice_enough), 0);
    EXPECT_EQ(getpriority(PRIO_PROCESS, 0), -20);
  });
}

TEST(SetPriorityTest, RootCanExceedRLimits) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  // Not the min, not the max, otherwise arbitrary.
  constexpr int niceness_rlimit_cur = 17;
  constexpr int niceness_rlimit_max = niceness_rlimit_cur + 2;

  test_helper::ForkHelper fork_helper;
  Rendezvous ready = MakeRendezvous();
  Rendezvous complete = MakeRendezvous();

  pid_t target_pid =
      SpawnTarget(fork_helper, std::move(ready.poker), std::move(complete.holder), []() {
        struct rlimit limit = {
            .rlim_cur = static_cast<rlim_t>(niceness_rlimit_cur),
            .rlim_max = static_cast<rlim_t>(niceness_rlimit_max),
        };
        SAFE_SYSCALL(setrlimit(RLIMIT_NICE, &limit));

        Become(kUser1Uid, kUser1Uid);
      });

  ready.holder.hold();

  for (int niceness = 19; niceness >= -20; niceness--) {
    EXPECT_EQ(setpriority(PRIO_PROCESS, target_pid, niceness), 0);
    errno = 0;
    EXPECT_EQ(getpriority(PRIO_PROCESS, target_pid), niceness);
    EXPECT_EQ(errno, 0);
  }

  complete.poker.poke();
}

TEST(SetPriorityTest, RLimitedAndUnfriendly) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  // Not the min, not the max, otherwise arbitrary.
  constexpr int niceness_rlimit_cur = 12;
  constexpr int niceness_rlimit_max = niceness_rlimit_cur + 3;
  constexpr int within_rlimit_niceness = 18;
  constexpr int beyond_rlimit_niceness = -3;

  test_helper::ForkHelper fork_helper;
  Rendezvous ready = MakeRendezvous();
  Rendezvous complete = MakeRendezvous();

  pid_t target_pid =
      SpawnTarget(fork_helper, std::move(ready.poker), std::move(complete.holder), []() {
        struct rlimit limit = {
            .rlim_cur = static_cast<rlim_t>(niceness_rlimit_cur),
            .rlim_max = static_cast<rlim_t>(niceness_rlimit_max),
        };
        SAFE_SYSCALL(setrlimit(RLIMIT_NICE, &limit));
        sched_param param = {.sched_priority = 0};
        SAFE_SYSCALL(sched_setscheduler(0, SCHED_OTHER, &param));
        SAFE_SYSCALL(setpriority(PRIO_PROCESS, 0, within_rlimit_niceness));

        Become(kUser2Uid, kUser2Uid);
      });

  fork_helper.RunInForkedProcess([ready = std::move(ready.holder),
                                  complete = std::move(complete.poker), target_pid]() mutable {
    Become(kUser1Uid, kUser1Uid);

    ready.hold();

    // It's the unfriendliness that "wins"; attempting to set the niceness fails
    // with EPERM and the rlimit doesn't get a chance to fail with EACCES.
    errno = 0;
    EXPECT_EQ(setpriority(PRIO_PROCESS, target_pid, beyond_rlimit_niceness), -1);
    EXPECT_EQ(errno, EPERM);

    complete.poke();
  });
}

TEST(SchedGetSchedulerTest, RootCanGetOwnScheduler) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  EXPECT_GE(sched_getscheduler(0), 0);
}

TEST(SchedGetSchedulerTest, RootCanGetSchedulerOfRootOwnedProcess) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper fork_helper;
  Rendezvous complete = MakeRendezvous();

  pid_t target_pid = fork_helper.RunInForkedProcess(
      [complete = std::move(complete.holder)]() mutable { complete.hold(); });

  EXPECT_GE(sched_getscheduler(target_pid), 0);

  complete.poker.poke();
}

TEST(SchedGetSchedulerTest, RootCanGetSchedulerOfNonRootOwnedProcess) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper fork_helper;
  Rendezvous ready = MakeRendezvous();
  Rendezvous complete = MakeRendezvous();

  pid_t target_pid = SpawnTarget(fork_helper, std::move(ready.poker), std::move(complete.holder),
                                 []() { Become(kUser1Uid, kUser1Uid); });

  ready.holder.hold();

  EXPECT_GE(sched_getscheduler(target_pid), 0);

  complete.poker.poke();
}

TEST(SchedGetSchedulerTest, NonRootCanGetOwnScheduler) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() {
    Become(kUser1Uid, kUser1Uid);

    EXPECT_GE(sched_getscheduler(0), 0);
  });
}

TEST(SchedGetSchedulerTest, NonRootCanGetSchedulerOfFriendlyProcess) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() {
    Become(kUser1Uid, kUser1Uid);

    test_helper::ForkHelper target_process_fork_helper;
    Rendezvous complete = MakeRendezvous();

    pid_t target_pid = target_process_fork_helper.RunInForkedProcess(
        [complete = std::move(complete.holder)]() mutable { complete.hold(); });

    EXPECT_GE(sched_getscheduler(target_pid), 0);

    complete.poker.poke();
  });
}

TEST(SchedGetSchedulerTest, NonRootCanGetSchedulerOfUnfriendlyProcess) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper fork_helper;
  Rendezvous ready = MakeRendezvous();
  Rendezvous complete = MakeRendezvous();

  pid_t target_pid = SpawnTarget(fork_helper, std::move(ready.poker), std::move(complete.holder),
                                 []() { Become(kUser2Uid, kUser2Uid); });

  fork_helper.RunInForkedProcess([ready = std::move(ready.holder),
                                  complete = std::move(complete.poker), target_pid]() mutable {
    Become(kUser1Uid, kUser1Uid);

    ready.hold();

    EXPECT_GE(sched_getscheduler(target_pid), 0);

    complete.poke();
  });
}

TEST(SchedGetSchedulerTest, ResetOnForkAppearsInReturnedValue) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() {
    sched_param param = {.sched_priority = 0};
    EXPECT_EQ(sched_setscheduler(0, SCHED_BATCH | SCHED_RESET_ON_FORK, &param), 0);

    EXPECT_EQ(sched_getscheduler(0), SCHED_BATCH | SCHED_RESET_ON_FORK);
  });
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

TEST(SchedSetSchedulerTest, InvalidArguments) {
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

TEST(SchedSetSchedulerTest, RootCanSetOwnScheduler) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() { EXPECT_TRUE(SetScheduler(0)); });
}

TEST(SchedSetSchedulerTest, RootCanSetSchedulerOfRootOwnedProcess) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() {
    test_helper::ForkHelper target_process_fork_helper;
    Rendezvous complete = MakeRendezvous();

    pid_t target_pid = target_process_fork_helper.RunInForkedProcess(
        [complete = std::move(complete.holder)]() mutable { complete.hold(); });

    EXPECT_TRUE(SetScheduler(target_pid));

    complete.poker.poke();
  });
}

TEST(SchedSetSchedulerTest, RootCanSetSchedulerOfNonRootOwnedProcess) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() {
    test_helper::ForkHelper target_process_fork_helper;
    Rendezvous ready = MakeRendezvous();
    Rendezvous complete = MakeRendezvous();

    pid_t target_pid =
        SpawnTarget(target_process_fork_helper, std::move(ready.poker), std::move(complete.holder),
                    []() { Become(kUser1Uid, kUser1Uid); });

    ready.holder.hold();

    EXPECT_TRUE(SetScheduler(target_pid));

    complete.poker.poke();
  });
}

TEST(SchedSetSchedulerTest, NonRootCanSetOwnScheduler) {
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

    Become(kUser1Uid, kUser1Uid);

    EXPECT_TRUE(SetScheduler(0));
  });
}

TEST(SchedSetSchedulerTest, NonRootCanSetSchedulerOfFriendlyProcess) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper fork_helper;
  Rendezvous ready = MakeRendezvous();
  Rendezvous complete = MakeRendezvous();

  pid_t target_pid =
      SpawnTarget(fork_helper, std::move(ready.poker), std::move(complete.holder), []() {
        // Remove rlimits from consideration in this test.
        struct rlimit limit = {
            .rlim_cur = RLIM_INFINITY,
            .rlim_max = RLIM_INFINITY,
        };
        SAFE_SYSCALL(setrlimit(RLIMIT_RTPRIO, &limit));

        Become(kUser1Uid, kUser1Uid);
      });

  fork_helper.RunInForkedProcess([ready = std::move(ready.holder),
                                  complete = std::move(complete.poker), target_pid]() mutable {
    Become(kUser1Uid, kUser1Uid);

    ready.hold();

    EXPECT_TRUE(SetScheduler(target_pid));

    complete.poke();
  });
}

TEST(SchedSetSchedulerTest, RootCanClearResetOnFork) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() {
    sched_param param = {.sched_priority = 2};

    EXPECT_EQ(sched_setscheduler(0, SCHED_FIFO | SCHED_RESET_ON_FORK, &param), 0);
    EXPECT_EQ(sched_getscheduler(0), SCHED_FIFO | SCHED_RESET_ON_FORK);

    EXPECT_EQ(sched_setscheduler(0, SCHED_FIFO, &param), 0);
    EXPECT_EQ(sched_getscheduler(0), SCHED_FIFO);
  });
}

TEST(SchedSetSchedulerTest, ResetOnForkShiftsToNonRealTimePolicy) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() {
    sched_param param = {.sched_priority = 2};
    EXPECT_EQ(sched_setscheduler(0, SCHED_FIFO | SCHED_RESET_ON_FORK, &param), 0);

    Become(kUser1Uid, kUser1Uid);

    test_helper::ForkHelper target_process_fork_helper;
    Rendezvous complete = MakeRendezvous();

    pid_t target_pid = target_process_fork_helper.RunInForkedProcess(
        [complete = std::move(complete.holder)]() mutable { complete.hold(); });

    EXPECT_EQ(sched_getscheduler(target_pid), SCHED_OTHER);

    complete.poker.poke();
  });
}

TEST(SchedSetSchedulerTest, ResetOnForkShiftsToNonRealTimePolicyEvenForRoot) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() {
    sched_param param = {.sched_priority = 2};
    EXPECT_EQ(sched_setscheduler(0, SCHED_FIFO | SCHED_RESET_ON_FORK, &param), 0);

    // No transition to an ordinary user here; we remain root!

    test_helper::ForkHelper target_process_fork_helper;
    Rendezvous complete = MakeRendezvous();

    pid_t target_pid = target_process_fork_helper.RunInForkedProcess(
        [complete = std::move(complete.holder)]() mutable { complete.hold(); });

    EXPECT_EQ(sched_getscheduler(target_pid), SCHED_OTHER);

    complete.poker.poke();
  });
}

TEST(SchedSetSchedulerTest, ResetOnForkZeroesNegativeNiceness) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() {
    sched_param param = {.sched_priority = 0};
    EXPECT_EQ(sched_setscheduler(0, SCHED_BATCH | SCHED_RESET_ON_FORK, &param), 0);
    EXPECT_EQ(setpriority(PRIO_PROCESS, 0, -10), 0);

    Become(kUser1Uid, kUser1Uid);

    test_helper::ForkHelper target_process_fork_helper;
    Rendezvous complete = MakeRendezvous();

    pid_t target_pid = target_process_fork_helper.RunInForkedProcess(
        [complete = std::move(complete.holder)]() mutable { complete.hold(); });

    EXPECT_EQ(sched_getscheduler(target_pid), SCHED_BATCH);
    EXPECT_EQ(getpriority(PRIO_PROCESS, target_pid), 0);

    complete.poker.poke();
  });
}

TEST(SchedGetParamTest, RootCanGetOwnParam) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  sched_param observed_param;
  observed_param.sched_priority = -1;

  EXPECT_EQ(sched_getparam(0, &observed_param), 0);
  EXPECT_GE(observed_param.sched_priority, 0);
}

TEST(SchedGetParamTest, RootCanGetParamOfRootOwnedProcess) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper fork_helper;
  Rendezvous complete = MakeRendezvous();

  pid_t target_pid = fork_helper.RunInForkedProcess(
      [complete = std::move(complete.holder)]() mutable { complete.hold(); });

  sched_param observed_param;
  observed_param.sched_priority = -1;

  EXPECT_EQ(sched_getparam(target_pid, &observed_param), 0);
  EXPECT_GE(observed_param.sched_priority, 0);

  complete.poker.poke();
}

TEST(SchedGetParamTest, RootCanGetParamOfNonRootOwnedProcess) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper fork_helper;
  Rendezvous ready = MakeRendezvous();
  Rendezvous complete = MakeRendezvous();

  pid_t target_pid = SpawnTarget(fork_helper, std::move(ready.poker), std::move(complete.holder),
                                 []() { Become(kUser1Uid, kUser1Uid); });

  ready.holder.hold();

  sched_param observed_param;
  observed_param.sched_priority = -1;

  EXPECT_EQ(sched_getparam(target_pid, &observed_param), 0);
  EXPECT_GE(observed_param.sched_priority, 0);

  complete.poker.poke();
}

TEST(SchedGetParamTest, NonRootCanGetOwnScheduler) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() {
    Become(kUser1Uid, kUser1Uid);

    sched_param observed_param;
    observed_param.sched_priority = -1;

    EXPECT_EQ(sched_getparam(0, &observed_param), 0);
    EXPECT_GE(observed_param.sched_priority, 0);
  });
}

TEST(SchedGetParamTest, NonRootCanGetParamOfFriendlyProcess) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() {
    Become(kUser1Uid, kUser1Uid);

    test_helper::ForkHelper target_process_fork_helper;
    Rendezvous complete = MakeRendezvous();

    pid_t target_pid = target_process_fork_helper.RunInForkedProcess(
        [complete = std::move(complete.holder)]() mutable { complete.hold(); });

    sched_param observed_param;
    observed_param.sched_priority = -1;

    EXPECT_EQ(sched_getparam(target_pid, &observed_param), 0);
    EXPECT_GE(observed_param.sched_priority, 0);

    complete.poker.poke();
  });
}

TEST(SchedGetParamTest, NonRootCanGetParamOfUnfriendlyProcess) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper fork_helper;
  Rendezvous ready = MakeRendezvous();
  Rendezvous complete = MakeRendezvous();

  pid_t target_pid = SpawnTarget(fork_helper, std::move(ready.poker), std::move(complete.holder),
                                 []() { Become(kUser2Uid, kUser2Uid); });

  fork_helper.RunInForkedProcess([ready = std::move(ready.holder),
                                  complete = std::move(complete.poker), target_pid]() mutable {
    Become(kUser1Uid, kUser1Uid);

    ready.hold();

    sched_param observed_param;
    observed_param.sched_priority = -1;

    EXPECT_EQ(sched_getparam(target_pid, &observed_param), 0);
    EXPECT_GE(observed_param.sched_priority, 0);

    complete.poke();
  });
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

TEST(SchedSetParamTest, InvalidArguments) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() {
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
  });
}

TEST(SchedSetParamTest, RootCanSetOwnScheduler) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() { EXPECT_TRUE(SetParam(0)); });
}

TEST(SchedSetParamTest, RootCanSetParamOfRootOwnedProcess) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() {
    test_helper::ForkHelper target_process_fork_helper;
    Rendezvous complete = MakeRendezvous();

    pid_t target_pid = target_process_fork_helper.RunInForkedProcess(
        [complete = std::move(complete.holder)]() mutable { complete.hold(); });

    EXPECT_TRUE(SetParam(target_pid));

    complete.poker.poke();
  });
}

TEST(SchedSetParamTest, RootCanSetParamOfNonRootOwnedProcess) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper().RunInForkedProcess([]() {
    test_helper::ForkHelper target_process_fork_helper;
    Rendezvous ready = MakeRendezvous();
    Rendezvous complete = MakeRendezvous();

    pid_t target_pid =
        SpawnTarget(target_process_fork_helper, std::move(ready.poker), std::move(complete.holder),
                    []() { Become(kUser1Uid, kUser1Uid); });

    ready.holder.hold();

    EXPECT_TRUE(SetParam(target_pid));

    complete.poker.poke();
  });
}

TEST(SchedSetParamTest, NonRootCanSetOwnParam) {
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

    Become(kUser1Uid, kUser1Uid);

    EXPECT_TRUE(SetParam(0));
  });
}

TEST(SchedSetParamTest, NonRootCanSetParamOfFriendlyProcess) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping test.";
  }

  test_helper::ForkHelper fork_helper;
  Rendezvous ready = MakeRendezvous();
  Rendezvous complete = MakeRendezvous();

  pid_t target_pid =
      SpawnTarget(fork_helper, std::move(ready.poker), std::move(complete.holder), []() {
        // Remove rlimits from consideration in this test.
        struct rlimit limit = {
            .rlim_cur = RLIM_INFINITY,
            .rlim_max = RLIM_INFINITY,
        };
        SAFE_SYSCALL(setrlimit(RLIMIT_RTPRIO, &limit));

        Become(kUser1Uid, kUser1Uid);
      });

  fork_helper.RunInForkedProcess([ready = std::move(ready.holder),
                                  complete = std::move(complete.poker), target_pid]() mutable {
    Become(kUser1Uid, kUser1Uid);

    ready.hold();

    EXPECT_TRUE(SetParam(target_pid));

    complete.poke();
  });
}

}  // namespace
