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

}  // namespace
