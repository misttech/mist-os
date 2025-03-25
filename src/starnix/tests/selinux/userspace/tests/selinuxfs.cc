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

ValidateContextResult ValidateContext(std::string_view context) {
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
      // Use `c_str()` to strip the trailing NUL, if any, from the read context.
      return fit::ok(RemoveTrailingNul(validated_context));
    }
    if (result < 0) {
      return fit::error(errno);
    }
    validated_context.append(read_buf, result);
  }
}

ValidateContextResult expect_ok(std::string_view context) { return fit::ok(context); }

TEST(SeLinuxFsContext, ValidatesRequiredFieldsPresent) {
  LoadPolicy("selinuxfs_policy.pp");

  // Contexts that have too few colons to provide user, role, type & sensitivity are rejected.
  EXPECT_EQ(ValidateContext("test_selinuxfs_u"), fit::failed());
  EXPECT_EQ(ValidateContext("test_selinuxfs_u:test_selinuxfs_r"), fit::failed());
  EXPECT_EQ(ValidateContext("test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t"), fit::failed());

  // The minimum valid context has at least user, role, type and low/default sensitivity.
  constexpr std::string_view kMinimumValidContext =
      "test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0";
  EXPECT_EQ(ValidateContext(kMinimumValidContext), expect_ok(kMinimumValidContext));
}

TEST(SeLinuxFsContext, ValidatesFieldValues) {
  LoadPolicy("selinuxfs_policy.pp");

  // Valid contexts are successfully written, and can be read-back.
  constexpr std::string_view kValidContext =
      "test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0:c0-s2:c0.c2";
  EXPECT_EQ(ValidateContext(kValidContext), expect_ok(kValidContext));

  // Context user must be defined by the policy.
  EXPECT_EQ(ValidateContext("bad_value:test_selinuxfs_r:test_selinuxfs_t:s0:c0-s2:c0.c2"),
            fit::failed());

  // Context role must be defined by the policy.
  EXPECT_EQ(ValidateContext("test_selinuxfs_u:bad_value:test_selinuxfs_t:s0:c0-s2:c0.c2"),
            fit::failed());

  // Context type/domain must be defined by the policy.
  EXPECT_EQ(ValidateContext("test_selinuxfs_u:test_selinuxfs_r:bad_value:s0:c0-s2:c0.c2"),
            fit::failed());

  // Context low & high sensitivities must be defined by the policy.
  EXPECT_EQ(
      ValidateContext("test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:bad_value:c0-s2:c0.c2"),
      fit::failed());
  EXPECT_EQ(
      ValidateContext("test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0:c0-bad_value:c0.c2"),
      fit::failed());

  // Context low & high categories must be defined by the policy.
  EXPECT_EQ(
      ValidateContext("test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0:bad_value-s2:c0.c2"),
      fit::failed());
  EXPECT_EQ(
      ValidateContext("test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0:c0-s2:c0.bad_value"),
      fit::failed());
}

TEST(SeLinuxFsContext, ValidatesAllowedUserFieldValues) {
  LoadPolicy("selinuxfs_policy.pp");

  // The "test_selinuxfs_u" user is granted the full range of categories.
  constexpr std::string_view kValidContext =
      "test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0:c0-s2:c0.c2";
  EXPECT_EQ(ValidateContext(kValidContext), expect_ok(kValidContext));

  // The "test_selinuxfs_limited_u" user is granted only "s0" sensitivity, must have "c0" category
  // and may have "c1" category.
  constexpr std::string_view kLimitedContext_Valid =
      "test_selinuxfs_limited_level_u:test_selinuxfs_r:test_selinuxfs_t:s0:c0";
  EXPECT_EQ(ValidateContext(kLimitedContext_Valid), expect_ok(kLimitedContext_Valid));

  constexpr std::string_view kLimitedContext_MissingCategory =
      "test_selinuxfs_limited_level_u:test_selinuxfs_r:test_selinuxfs_t:s0";
  EXPECT_EQ(ValidateContext(kLimitedContext_MissingCategory), fit::failed());

  constexpr std::string_view kLimitedContext_BadSensitivity =
      "test_selinuxfs_limited_level_u:test_selinuxfs_r:test_selinuxfs_t:s1:c0";
  EXPECT_EQ(ValidateContext(kLimitedContext_BadSensitivity), fit::failed());
}

TEST(SeLinuxFsContext, NormalizeCategories) {
  LoadPolicy("selinuxfs_policy.pp");

  // Expansion of a three-element category span results in the same Security Context as using the
  // range syntax directly.
  constexpr std::string_view kThreeCategoryContextFormA =
      "test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s2:c0.c2";
  constexpr std::string_view kThreeCategoryContextFormB =
      "test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s2:c0,c1,c2";
  EXPECT_EQ(ValidateContext(kThreeCategoryContextFormA),
            ValidateContext(kThreeCategoryContextFormB));

  // Using a pair of categories results in the same Security Context as a two-element range.
  constexpr std::string_view kTwoCategoryContextFormA =
      "test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s2:c0.c1";
  constexpr std::string_view kTwoCategoryContextFormB =
      "test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s2:c0,c1";
  EXPECT_EQ(ValidateContext(kTwoCategoryContextFormA), ValidateContext(kTwoCategoryContextFormB));
}

}  // namespace
