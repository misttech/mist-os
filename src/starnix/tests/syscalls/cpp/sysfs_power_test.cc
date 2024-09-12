// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <sys/epoll.h>
#include <sys/poll.h>
#include <sys/timerfd.h>
#include <sys/uio.h>

#include <regex>
#include <string>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/lib/files/directory.h"
#include "src/lib/files/file.h"
#include "src/starnix/tests/syscalls/cpp/test_helper.h"

using testing::IsSupersetOf;

namespace {

void VerifyReadOutOfBound(const std::string& path) {
  constexpr int kOffset = 100;
  constexpr int kSize = 10;
  char read_buffer[kSize];
  struct iovec iov[] = {
      {
          .iov_base = &read_buffer[0],
          .iov_len = kSize,
      },
  };
  int fd = openat(AT_FDCWD, path.c_str(), O_RDONLY);
  EXPECT_NE(-1, fd);
  EXPECT_EQ(0, preadv(fd, iov, std::size(iov), kOffset));
}

bool attempt_suspend(const std::string& suspend_type) {
  return files::WriteFile("/sys/power/state", suspend_type);
}

timer_t start_interval_timer() {
  timer_t timer_id;
  struct sigevent sev = {};
  sev.sigev_notify = SIGEV_NONE;
  EXPECT_NE(-1, timer_create(CLOCK_REALTIME_ALARM, &sev, &timer_id));

  // Set a test timer that will trigger every 1 second to wake
  // up all the subsequent suspends.
  struct itimerspec its = {};
  its.it_value.tv_sec = 2;
  its.it_interval.tv_sec = 2;
  EXPECT_NE(-1, timer_settime(timer_id, 0, &its, nullptr));

  return timer_id;
}

void stop_interval_timer(timer_t timer_id) {
  struct itimerspec its = {};
  its.it_value.tv_sec = 0;
  its.it_interval.tv_sec = 0;
  EXPECT_NE(-1, timer_settime(timer_id, 0, &its, nullptr));
}

}  // namespace

class SysfsPowerTest : public ::testing::Test {
 public:
  void SetUp() override {
    // TODO(https://fxbug.dev/317285180) don't skip on baseline
    // Assume starnix always has /sys/power and /sys/kernel/wakeup_reasons
    if (!test_helper::IsStarnix() && access("/sys/power", F_OK) == -1 &&
        access("/sys/kernel/wakeup_reasons", F_OK) == -1) {
      GTEST_SKIP() << "/sys/power not available, skipping...";
    }
  }
};

TEST_F(SysfsPowerTest, PowerDirectoryContainsExpectedContents) {
  std::vector<std::string> files;
  EXPECT_TRUE(files::ReadDirContents("/sys/power", &files));
  EXPECT_THAT(files, IsSupersetOf({"suspend_stats", "wakeup_count", "state", "sync_on_suspend"}));
}

TEST_F(SysfsPowerTest, WakeupReasonsDirectoryContainsExpectedContents) {
  std::vector<std::string> files;
  EXPECT_TRUE(files::ReadDirContents("/sys/kernel/wakeup_reasons", &files));
  EXPECT_THAT(files, IsSupersetOf({"last_resume_reason", "last_suspend_time"}));
}

TEST_F(SysfsPowerTest, SuspendStatsDirectoryContainsExpectedContents) {
  std::vector<std::string> suspend_stats_files;
  EXPECT_TRUE(files::ReadDirContents("/sys/power/suspend_stats", &suspend_stats_files));
  EXPECT_THAT(suspend_stats_files,
              IsSupersetOf({"success", "fail", "last_failed_dev", "last_failed_errno"}));
}

TEST_F(SysfsPowerTest, SuspendStatsFilesContainDefaultsSuccess) {
  std::string success_str;
  EXPECT_TRUE(files::ReadFileToString("/sys/power/suspend_stats/success", &success_str));
  EXPECT_TRUE(std::regex_match(success_str, std::regex("^[0-9]+\n")));
}

TEST_F(SysfsPowerTest, SuspendStatsFilesUpdateAfterSuccessfulSuspend) {
  std::string prev_success_str;
  EXPECT_TRUE(files::ReadFileToString("/sys/power/suspend_stats/success", &prev_success_str));
  std::string prev_fail_str;
  EXPECT_TRUE(files::ReadFileToString("/sys/power/suspend_stats/fail", &prev_fail_str));

  timer_t timer_id = start_interval_timer();
  ASSERT_TRUE(attempt_suspend("mem"));
  stop_interval_timer(timer_id);

  std::string new_success_str;
  std::string new_fail_str;
  EXPECT_TRUE(files::ReadFileToString("/sys/power/suspend_stats/success", &new_success_str));
  EXPECT_TRUE(files::ReadFileToString("/sys/power/suspend_stats/fail", &new_fail_str));
  EXPECT_EQ(std::atoi(prev_success_str.c_str()) + 1, std::atoi(new_success_str.c_str()));
  EXPECT_EQ(std::atoi(prev_fail_str.c_str()), std::atoi(new_fail_str.c_str()));
}

TEST_F(SysfsPowerTest, SuspendStatsFilesUpdateAfterFailedSuspend) {
  std::string prev_success_str;
  EXPECT_TRUE(files::ReadFileToString("/sys/power/suspend_stats/success", &prev_success_str));
  std::string prev_fail_str;
  EXPECT_TRUE(files::ReadFileToString("/sys/power/suspend_stats/fail", &prev_fail_str));

  fbl::unique_fd timer_fd(timerfd_create(CLOCK_REALTIME, 0));
  EXPECT_TRUE(timer_fd.is_valid());

  fbl::unique_fd epoll_fd(epoll_create(1));
  EXPECT_TRUE(epoll_fd.is_valid());

  struct epoll_event ev = epoll_event();
  ev.events = EPOLLIN | EPOLLWAKEUP;
  EXPECT_EQ(0, epoll_ctl(epoll_fd.get(), EPOLL_CTL_ADD, timer_fd.get(), &ev));

  // Activate the EPOLLWAKEUP event to create the implicit wake lock.
  {
    struct itimerspec its = {};
    its.it_value.tv_sec = 1;
    EXPECT_EQ(0, timerfd_settime(timer_fd.get(), 0, &its, nullptr));

    int ret = 0;
    struct epoll_event out_ev;
    ret = epoll_wait(epoll_fd.get(), &out_ev, 1, -1);
    EXPECT_EQ(1, ret);

    uint64_t val = 0;
    // Read the event from the timer_fd to reset the pending events.
    EXPECT_EQ(1, read(timer_fd.get(), &val, 1));
  }

  // This should fail due to the implicit wake lock.
  ASSERT_FALSE(attempt_suspend("mem"));

  std::string new_success_str;
  std::string new_fail_str;
  EXPECT_TRUE(files::ReadFileToString("/sys/power/suspend_stats/success", &new_success_str));
  EXPECT_TRUE(files::ReadFileToString("/sys/power/suspend_stats/fail", &new_fail_str));
  EXPECT_EQ(std::atoi(prev_success_str.c_str()), std::atoi(new_success_str.c_str()));
  EXPECT_EQ(std::atoi(prev_fail_str.c_str()) + 1, std::atoi(new_fail_str.c_str()));

  // Wait on the events again, which should clear the EPOLLWAKEUP when
  // no events are returned.
  {
    int ret = 0;
    struct epoll_event out_ev;
    ret = epoll_wait(epoll_fd.get(), &out_ev, 1, 0);
    EXPECT_EQ(0, ret);
  }
}

TEST_F(SysfsPowerTest, SuspendStatsFilesContainDefaultsFail) {
  std::string fail_str;
  EXPECT_TRUE(files::ReadFileToString("/sys/power/suspend_stats/fail", &fail_str));
  EXPECT_TRUE(std::regex_match(fail_str, std::regex("^[0-9]+\n$")));
}

TEST_F(SysfsPowerTest, SuspendStatsFilesContainDefaultsLastFailedDev) {
  std::string last_failed_dev_str;
  EXPECT_TRUE(
      files::ReadFileToString("/sys/power/suspend_stats/last_failed_dev", &last_failed_dev_str));
  EXPECT_TRUE(std::regex_match(last_failed_dev_str, std::regex("^.*\n$")));
}

TEST_F(SysfsPowerTest, SuspendStatsFilesContainDefaultsLastFailedErrno) {
  std::string last_failed_errno_str;
  EXPECT_TRUE(files::ReadFileToString("/sys/power/suspend_stats/last_failed_errno",
                                      &last_failed_errno_str));
  // These tests always run in an environment where suspends don't fail.
  std::regex pattern("^(0|-?([1-9]\\d*|\\d))\n$");
  EXPECT_TRUE(std::regex_match(last_failed_errno_str, pattern));
}

TEST_F(SysfsPowerTest, WakeupCountFileContainsExpectedContents) {
  std::string wakeup_count_str;
  EXPECT_TRUE(files::ReadFileToString("/sys/power/wakeup_count", &wakeup_count_str));
  EXPECT_TRUE(std::regex_match(wakeup_count_str, std::regex("^[0-9]+\n$")));
}

TEST_F(SysfsPowerTest, WakeupCountFileReadOutofBound) {
  VerifyReadOutOfBound("/sys/power/wakeup_count");
}

TEST_F(SysfsPowerTest, WakeupCountFileWrite) {
  EXPECT_FALSE(files::WriteFile("/sys/power/wakeup_count", "test"));
  EXPECT_FALSE(files::WriteFile("/sys/power/wakeup_count", std::to_string(INT_MAX)));

  std::string wakeup_count_str;
  EXPECT_TRUE(files::ReadFileToString("/sys/power/wakeup_count", &wakeup_count_str));
  EXPECT_TRUE(files::WriteFile("/sys/power/wakeup_count", wakeup_count_str));
}

TEST_F(SysfsPowerTest, SuspendStateFileContainsExpectedContents) {
  std::string states_str;
  EXPECT_TRUE(files::ReadFileToString("/sys/power/state", &states_str));
  EXPECT_TRUE(std::regex_match(states_str, std::regex("([mem|freeze|disk|standby]\\s?)*\n$")));
}

TEST_F(SysfsPowerTest, SuspendStateFileReadOutOfBound) { VerifyReadOutOfBound("/sys/power/state"); }

TEST_F(SysfsPowerTest, SuspendStateFileWrite) {
  timer_t timer_id = start_interval_timer();

  ASSERT_TRUE(attempt_suspend("mem"));
  ASSERT_TRUE(attempt_suspend("mem\n"));
  ASSERT_TRUE(attempt_suspend("mem\nfoobar"));
  ASSERT_TRUE(attempt_suspend("freeze"));
  ASSERT_TRUE(attempt_suspend("freeze\n"));
  ASSERT_TRUE(attempt_suspend("freeze\nfoobar"));

  stop_interval_timer(timer_id);
}

TEST_F(SysfsPowerTest, SuspendStateFileWriteFailsWaitAgain) {
  fbl::unique_fd timer_fd(timerfd_create(CLOCK_REALTIME, 0));
  EXPECT_TRUE(timer_fd.is_valid());

  fbl::unique_fd epoll_fd(epoll_create(1));
  EXPECT_TRUE(epoll_fd.is_valid());

  struct epoll_event ev = epoll_event();
  ev.events = EPOLLIN | EPOLLWAKEUP;
  EXPECT_EQ(0, epoll_ctl(epoll_fd.get(), EPOLL_CTL_ADD, timer_fd.get(), &ev));

  // Activate the EPOLLWAKEUP event to create the implicit wake lock.
  {
    struct itimerspec its = {};
    its.it_value.tv_sec = 1;
    EXPECT_EQ(0, timerfd_settime(timer_fd.get(), 0, &its, nullptr));

    int ret = 0;
    struct epoll_event out_ev;
    ret = epoll_wait(epoll_fd.get(), &out_ev, 1, -1);
    EXPECT_EQ(1, ret);

    uint64_t val = 0;
    // Read the event from the timer_fd to reset the pending events.
    EXPECT_EQ(1, read(timer_fd.get(), &val, 1));
  }

  // This should fail due to the implicit wake lock.
  ASSERT_FALSE(attempt_suspend("mem"));

  // Wait on the events again, which should clear the EPOLLWAKEUP when
  // no events are returned.
  {
    int ret = 0;
    struct epoll_event out_ev;
    ret = epoll_wait(epoll_fd.get(), &out_ev, 1, 0);
    EXPECT_EQ(0, ret);
  }

  // Attempt to suspend the system now that the implicit wake lock
  // should have been deleted.
  timer_t timer_id = start_interval_timer();
  ASSERT_TRUE(attempt_suspend("mem"));
  stop_interval_timer(timer_id);
}

TEST_F(SysfsPowerTest, SuspendStateFileWriteFailsCloseFD) {
  fbl::unique_fd timer_fd(timerfd_create(CLOCK_REALTIME, 0));
  EXPECT_TRUE(timer_fd.is_valid());

  fbl::unique_fd epoll_fd(epoll_create(1));
  EXPECT_TRUE(epoll_fd.is_valid());

  struct epoll_event ev = epoll_event();
  ev.events = EPOLLIN | EPOLLWAKEUP;
  EXPECT_EQ(0, epoll_ctl(epoll_fd.get(), EPOLL_CTL_ADD, timer_fd.get(), &ev));

  // Activate the EPOLLWAKEUP event to create the implicit wake lock.
  {
    struct itimerspec its = {};
    its.it_value.tv_sec = 1;
    EXPECT_EQ(0, timerfd_settime(timer_fd.get(), 0, &its, nullptr));

    int ret = 0;
    struct epoll_event out_ev;
    ret = epoll_wait(epoll_fd.get(), &out_ev, 1, -1);
    EXPECT_EQ(1, ret);

    uint64_t val = 0;
    // Read the event from the timer_fd to reset the pending events.
    EXPECT_EQ(1, read(timer_fd.get(), &val, 1));
  }

  // This should fail due to the implicit wake lock.
  ASSERT_FALSE(attempt_suspend("mem"));

  // Closing the epoll file descriptor should remove the wake lock.
  timer_fd.reset();

  // Attempt to suspend the system now that the implicit wake lock
  // should have been deleted.
  timer_t timer_id = start_interval_timer();
  ASSERT_TRUE(attempt_suspend("mem"));
  stop_interval_timer(timer_id);
}

TEST_F(SysfsPowerTest, SuspendStateFileWriteFailsCloseEpollFD) {
  fbl::unique_fd timer_fd(timerfd_create(CLOCK_REALTIME, 0));
  EXPECT_TRUE(timer_fd.is_valid());

  fbl::unique_fd epoll_fd(epoll_create(1));
  EXPECT_TRUE(epoll_fd.is_valid());

  struct epoll_event ev = epoll_event();
  ev.events = EPOLLIN | EPOLLWAKEUP;
  EXPECT_EQ(0, epoll_ctl(epoll_fd.get(), EPOLL_CTL_ADD, timer_fd.get(), &ev));

  // Activate the EPOLLWAKEUP event to create the implicit wake lock.
  {
    struct itimerspec its = {};
    its.it_value.tv_sec = 1;
    EXPECT_EQ(0, timerfd_settime(timer_fd.get(), 0, &its, nullptr));

    int ret = 0;
    struct epoll_event out_ev;
    ret = epoll_wait(epoll_fd.get(), &out_ev, 1, -1);
    EXPECT_EQ(1, ret);

    uint64_t val = 0;
    // Read the event from the timer_fd to reset the pending events.
    EXPECT_EQ(1, read(timer_fd.get(), &val, 1));
  }

  // This should fail due to the implicit wake lock.
  ASSERT_FALSE(attempt_suspend("mem"));

  // Closing the epoll file descriptor should remove the wake lock.
  epoll_fd.reset();

  // Attempt to suspend the system now that the implicit wake lock
  // should have been deleted.
  timer_t timer_id = start_interval_timer();
  ASSERT_TRUE(attempt_suspend("mem"));
  stop_interval_timer(timer_id);
}

TEST_F(SysfsPowerTest, SuspendStateFileWriteFailsEpollDelete) {
  fbl::unique_fd timer_fd(timerfd_create(CLOCK_REALTIME, 0));
  EXPECT_TRUE(timer_fd.is_valid());

  fbl::unique_fd epoll_fd(epoll_create(1));
  EXPECT_TRUE(epoll_fd.is_valid());

  struct epoll_event ev = epoll_event();
  ev.events = EPOLLIN | EPOLLWAKEUP;
  EXPECT_EQ(0, epoll_ctl(epoll_fd.get(), EPOLL_CTL_ADD, timer_fd.get(), &ev));

  // Activate the EPOLLWAKEUP event to create the implicit wake lock.
  {
    struct itimerspec its = {};
    its.it_value.tv_sec = 1;
    EXPECT_EQ(0, timerfd_settime(timer_fd.get(), 0, &its, nullptr));

    int ret = 0;
    struct epoll_event out_ev;
    ret = epoll_wait(epoll_fd.get(), &out_ev, 1, -1);
    EXPECT_EQ(1, ret);

    uint64_t val = 0;
    // Read the event from the timer_fd to reset the pending events.
    EXPECT_EQ(1, read(timer_fd.get(), &val, 1));
  }

  // This should fail due to the implicit wake lock.
  ASSERT_FALSE(attempt_suspend("mem"));

  // Deleting the epoll file descriptor should remove the wake lock.
  EXPECT_EQ(0, epoll_ctl(epoll_fd.get(), EPOLL_CTL_DEL, timer_fd.get(), &ev));

  // Attempt to suspend the system now that the implicit wake lock
  // should have been deleted.
  timer_t timer_id = start_interval_timer();
  ASSERT_TRUE(attempt_suspend("mem"));
  stop_interval_timer(timer_id);
}

TEST_F(SysfsPowerTest, SuspendStateFileWriteInvalidFails) {
  ASSERT_FALSE(files::WriteFile("/sys/power/state", "test"));
  ASSERT_FALSE(files::WriteFile("/sys/power/state", "disk"));
}

TEST_F(SysfsPowerTest, SyncOnSuspendFileContainsExpectedContents) {
  std::string sync_on_suspend_str;
  EXPECT_TRUE(files::ReadFileToString("/sys/power/sync_on_suspend", &sync_on_suspend_str));
  EXPECT_TRUE(std::regex_match(sync_on_suspend_str, std::regex("(0|1)\n")));
}

TEST_F(SysfsPowerTest, SyncOnSuspendFileReadOutOfBound) {
  VerifyReadOutOfBound("/sys/power/sync_on_suspend");
}

TEST_F(SysfsPowerTest, SyncOnSuspendFileWrite) {
  EXPECT_FALSE(files::WriteFile("/sys/power/sync_on_suspend", "test"));
  EXPECT_FALSE(files::WriteFile("/sys/power/sync_on_suspend", std::to_string(2)));
  EXPECT_TRUE(files::WriteFile("/sys/power/sync_on_suspend", std::to_string(0)));
}

TEST_F(SysfsPowerTest, LastSuspendTimeFileContainsExpectedContents) {
  std::string last_suspend_time_str;
  EXPECT_TRUE(files::ReadFileToString("/sys/kernel/wakeup_reasons/last_suspend_time",
                                      &last_suspend_time_str));
  std::regex pattern("^(0|-?([1-9]\\d*|\\d)\\.\\d+)\\s(0|-?([1-9]\\d*|\\d)\\.\\d+)\n$");
  EXPECT_TRUE(std::regex_match(last_suspend_time_str, pattern));
}

TEST_F(SysfsPowerTest, LastResumeReasonFileContainsExpectedContents) {
  std::string last_suspend_time_str;
  EXPECT_TRUE(files::ReadFileToString("/sys/kernel/wakeup_reasons/last_resume_reason",
                                      &last_suspend_time_str));
  EXPECT_TRUE(std::regex_match(last_suspend_time_str, std::regex("^.*\n$")));
}
