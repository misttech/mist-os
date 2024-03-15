// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <string>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/lib/files/directory.h"
#include "src/lib/files/file.h"
#include "src/starnix/tests/syscalls/cpp/test_helper.h"

using testing::ContainsRegex;
using testing::IsSupersetOf;
using testing::MatchesRegex;

class SysfsPowerTest : public ::testing::Test {
 public:
  void SetUp() override {
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
  EXPECT_THAT(success_str, ContainsRegex("^[0-9]+\n"));
}

TEST_F(SysfsPowerTest, SuspendStatsFilesContainDefaultsFail) {
  std::string fail_str;
  EXPECT_TRUE(files::ReadFileToString("/sys/power/suspend_stats/fail", &fail_str));
  EXPECT_THAT(fail_str, ContainsRegex("^[0-9]+\n"));
}

TEST_F(SysfsPowerTest, SuspendStatsFilesContainDefaultsLastFailedDev) {
  std::string last_failed_dev_str;
  EXPECT_TRUE(
      files::ReadFileToString("/sys/power/suspend_stats/last_failed_dev", &last_failed_dev_str));
  EXPECT_THAT(last_failed_dev_str, ContainsRegex("^.*\n"));
}

TEST_F(SysfsPowerTest, SuspendStatsFilesContainDefaultsLastFailedErrno) {
  std::string last_failed_errno_str;
  EXPECT_TRUE(files::ReadFileToString("/sys/power/suspend_stats/last_failed_errno",
                                      &last_failed_errno_str));
  EXPECT_THAT(last_failed_errno_str, ContainsRegex("^(-[0-9]+|0)?\n"));
}

TEST_F(SysfsPowerTest, WakeupCountFileContainsExpectedContents) {
  std::string wakeup_count_str;
  EXPECT_TRUE(files::ReadFileToString("/sys/power/wakeup_count", &wakeup_count_str));
  EXPECT_THAT(wakeup_count_str, ContainsRegex("^[0-9]+\n"));
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
  EXPECT_THAT(states_str, MatchesRegex("([mem|freeze|disk|standby]\\s?)*\n"));
}

TEST_F(SysfsPowerTest, SuspendStateFileWrite) {
  ASSERT_TRUE(files::WriteFile("/sys/power/state", "mem"));
}

TEST_F(SysfsPowerTest, SuspendStateFileWriteFails) {
  ASSERT_FALSE(files::WriteFile("/sys/power/state", "test"));
  ASSERT_FALSE(files::WriteFile("/sys/power/state", "disk"));
}

TEST_F(SysfsPowerTest, SyncOnSuspendFileContainsExpectedContents) {
  std::string sync_on_suspend_str;
  EXPECT_TRUE(files::ReadFileToString("/sys/power/sync_on_suspend", &sync_on_suspend_str));
  EXPECT_THAT(sync_on_suspend_str, ContainsRegex("(0|1)\n"));
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
  EXPECT_THAT(last_suspend_time_str, ContainsRegex("^[0-9]+\\s[0-9]+\n"));
}

TEST_F(SysfsPowerTest, LastResumeReasonFileContainsExpectedContents) {
  std::string last_suspend_time_str;
  EXPECT_TRUE(files::ReadFileToString("/sys/kernel/wakeup_reasons/last_resume_reason",
                                      &last_suspend_time_str));
  EXPECT_THAT(last_suspend_time_str, ContainsRegex("^.*\n"));
}
