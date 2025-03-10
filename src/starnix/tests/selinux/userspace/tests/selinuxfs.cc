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

// TODO: Pretty-print without polluting the global namespace!
namespace fit {
void PrintTo(const fit::result<int, std::string>& value, std::ostream* os) {
  if (value.is_ok()) {
    *os << "Ok(\"" << value.value() << "\")";
  } else {
    *os << "Errno(" << value.error_value() << ")";
  }
}
}  // namespace fit

namespace {

using ValidateContextResult = fit::result<int, std::string>;

ValidateContextResult validate_context(std::string_view context) {
  constexpr char context_api_path[] = "/sys/fs/selinux/context";
  fbl::unique_fd context_api(open(context_api_path, O_RDWR));
  if (!context_api.is_valid()) {
    return fit::error(errno);
  }
  if (write(context_api.get(), context.data(), context.length()) < 0) {
    return fit::error(errno);
  }
  std::string validated_context;
  char read_buf[10];
  while (true) {
    ssize_t result = read(context_api.get(), read_buf, sizeof(read_buf));
    if (result == 0) {
      return fit::ok(validated_context);
    }
    if (result < 0) {
      return fit::error(errno);
    }
    validated_context.append(read_buf, result);
  }
}

ValidateContextResult expect_ok(std::string_view context) { return fit::ok(context); }

TEST(SeLinuxFsContext, ValidatesRequiredFieldsPresent) {
  LoadPolicy("minimal_policy.pp");

  // Contexts that have too few colons to provide user, role, type & sensitivity are rejected.
  EXPECT_EQ(validate_context("unconfined_u"), fit::failed());
  EXPECT_EQ(validate_context("unconfined_u:unconfined_r"), fit::failed());
  EXPECT_EQ(validate_context("unconfined_u:unconfined_r:unconfined_t"), fit::failed());

  // The minimum valid context has at least user, role, type and low/default sensitivity.
  constexpr std::string_view kMinimumValidContext = "unconfined_u:unconfined_r:unconfined_t:s0";
  EXPECT_EQ(validate_context(kMinimumValidContext), expect_ok(kMinimumValidContext));
}

TEST(SeLinuxFsContext, ValidatesFieldValues) {
  LoadPolicy("minimal_policy.pp");

  // Valid contexts are successfully written, and can be read-back.
  constexpr std::string_view kValidContext =
      "unconfined_u:unconfined_r:unconfined_t:s0:c0-s0:c0.c1";
  EXPECT_EQ(validate_context(kValidContext), expect_ok(kValidContext));

  // Context user must be defined by the policy.
  EXPECT_EQ(validate_context("bad_value:unconfined_r:unconfined_t:s0:c0-s0:c0.c1"), fit::failed());

  // Context role must be defined by the policy.
  EXPECT_EQ(validate_context("unconfined_u:bad_value:unconfined_t:s0:c0-s0:c0.c1"), fit::failed());

  // Context type/domain must be defined by the policy.
  EXPECT_EQ(validate_context("unconfined_u:unconfined_r:bad_value:s0:c0-s0:c0.c1"), fit::failed());

  // Context low & high sensitivities must be defined by the policy.
  EXPECT_EQ(validate_context("unconfined_u:unconfined_r:unconfined_t:bad_value:c0-s0:c0.c1"),
            fit::failed());
  EXPECT_EQ(validate_context("unconfined_u:unconfined_r:unconfined_t:s0:c0-bad_value:c0.c1"),
            fit::failed());

  // Context low & high categories must be defined by the policy.
  EXPECT_EQ(validate_context("unconfined_u:unconfined_r:unconfined_t:s0:bad_value-s0:c0.c1"),
            fit::failed());
  EXPECT_EQ(validate_context("unconfined_u:unconfined_r:unconfined_t:s0:c0-s0:c0.bad_value"),
            fit::failed());
}

}  // namespace
