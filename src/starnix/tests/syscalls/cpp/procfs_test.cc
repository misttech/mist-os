// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <errno.h>
#include <fcntl.h>
#include <lib/fit/defer.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#include <fstream>
#include <string>

#include <gtest/gtest.h>
#include <src/lib/files/file.h>

#include "src/starnix/tests/syscalls/cpp/test_helper.h"

namespace {

using testing::ContainsRegex;
using testing::MatchesRegex;

class ProcfsTest : public ::testing::Test {
 protected:
  // Verifies whether the input string is a valid UUID in hyphenated format
  // Example:
  // af0af413-58e1-4210-bb57-bc9a9d3ca44a
  // Segment lengths:
  //     8   |  4 |  4 |  4 |   12
  void is_valid_uuid(std::string in) {
    size_t pos = 0;
    std::string token;
    std::array<size_t, 5> lengths = {8, 4, 4, 4, 12};  // Lengths of tokens
    // Some Linux kernels have a newline character at the end
    if (in[in.size() - 1] == '\n') {
      in.pop_back();
    }
    in.append("-");

    // For each hyphen-delimited token of the UUID string
    for (size_t length : lengths) {
      // Grab token
      ASSERT_TRUE((pos = in.find("-")) != std::string::npos);
      token = in.substr(0, pos);
      in.erase(0, pos + 1);
      ASSERT_TRUE(length == token.size());

      // All characters are hexadecimal digits
      ASSERT_TRUE(std::all_of(token.begin(), token.end(), [](char c) { return std::isxdigit(c); }));
    }
  }
};

// Verify /proc/sys/kernel/random/boot_id exists and has the boot UUID
TEST_F(ProcfsTest, ProcSysKernelRandomBootIdExists) {
  std::string uuid;
  EXPECT_EQ(0, access("/proc/sys/kernel/random/boot_id", R_OK));
  EXPECT_TRUE(files::ReadFileToString("/proc/sys/kernel/random/boot_id", &uuid));
  is_valid_uuid(uuid);
}

// Verify /proc/pressure/{cpu,io,memory} contains something reasonable.
TEST_F(ProcfsTest, ProcPressure) {
  for (auto path : {"/proc/pressure/cpu", "/proc/pressure/io", "/proc/pressure/memory"}) {
    EXPECT_EQ(0, access(path, R_OK));
    std::string content;
    EXPECT_TRUE(files::ReadFileToString(path, &content));
    // Some systems does not eport the `full` statistics for CPU.
    EXPECT_THAT(content,
                MatchesRegex("some avg10=[0-9.]+ avg60=[0-9.]+ avg300=[0-9.]+ total=[0-9.]+\n"
                             "(full avg10=[0-9.]+ avg60=[0-9.]+ avg300=[0-9.]+ total=[0-9.]+\n)?"));
  }
}

// Verify that /proc/zoneinfo contains something reasonable.
TEST_F(ProcfsTest, ZoneInfo) {
  auto path = "/proc/zoneinfo";
  EXPECT_EQ(0, access(path, R_OK));
  std::string content;
  ASSERT_TRUE(files::ReadFileToString(path, &content));
  // Ensures that one node has `nr_inactive_file` and `nr_inactive_file`.
  EXPECT_THAT(content, ContainsRegex("(\n|^)Node [0-9]+, zone +[a-zA-Z]+\n"
                                     "  per-node stats\n"
                                     "( {6}.*\n)*"
                                     "      nr_inactive_file [0-9]+\n"
                                     "      nr_active_file [0-9]+\n"));
  // Ensures that one node has `free`, `min`, `low`, `high`, `present`, among others.
  EXPECT_THAT(content, ContainsRegex("(\n|^)Node [0-9]+, zone +[a-zA-Z]+\n"
                                     "(  .*\n)*"
                                     "  pages free     [0-9]+\n"
                                     "(    .*\n)*"
                                     "        min      [0-9]+\n"
                                     "(    .*\n)*"
                                     "        low      [0-9]+\n"
                                     "(    .*\n)*"
                                     "        high     [0-9]+\n"
                                     "(    .*\n)*"
                                     "        present  [0-9]+\n"
                                     "(    .*\n)*"
                                     "  pagesets\n"));
}

// Verify that /proc/vmstat shape is reasonable.
TEST_F(ProcfsTest, VmStatFile) {
  auto path = "/proc/vmstat";
  EXPECT_EQ(0, access(path, R_OK));
  std::string content;
  ASSERT_TRUE(files::ReadFileToString(path, &content));
  // Ensures that one node has `nr_inactive_file` and `nr_inactive_file`.
  EXPECT_THAT(content, ContainsRegex("(\n|^)workingset_refault_file [0-9]+\n"));
  EXPECT_THAT(content, ContainsRegex("(\n|^)nr_inactive_file [0-9]+\n"));
  EXPECT_THAT(content, ContainsRegex("(\n|^)nr_active_file [0-9]+\n"));
  EXPECT_THAT(content, ContainsRegex("(\n|^)pgscan_direct [0-9]+\n"));
  EXPECT_THAT(content, ContainsRegex("(\n|^)pgscan_kswapd [0-9]+\n"));
}

}  // namespace
