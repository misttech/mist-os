// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <lib/fit/result.h>
#include <unistd.h>

#include <ostream>
#include <string>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>
#include <linux/fs.h>

#include "src/starnix/tests/selinux/userspace/util.h"

namespace {

using ValidateContextResult = fit::result<int, std::string>;

ValidateContextResult get_procattr(std::string_view attr_name) {
  constexpr char procattr_prefix[] = "/proc/self/attr/";
  std::string attr_path(procattr_prefix);
  attr_path.append(attr_name);
  fbl::unique_fd attr_api(open(attr_path.c_str(), O_RDONLY));
  if (!attr_api.is_valid()) {
    return fit::error(errno);
  }
  std::string attr_value;
  char read_buf[10];
  while (true) {
    ssize_t result = read(attr_api.get(), read_buf, sizeof(read_buf));
    if (result == 0) {
      return fit::ok(attr_value);
    }
    if (result < 0) {
      return fit::error(errno);
    }
    attr_value.append(read_buf, result);
  }
}

TEST(ProcAttr, Current) {
  LoadPolicy("minimal_policy.pp");

  // Attempting to read the process' current context should return a value.
  auto current = get_procattr("current");
  EXPECT_TRUE(current.is_ok()) << "Unable to read 'current':" << current.error_value();
}

}  // namespace
