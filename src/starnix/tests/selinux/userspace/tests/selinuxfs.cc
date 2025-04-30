// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <lib/fit/result.h>
#include <unistd.h>

#include <string>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>
#include <linux/fs.h>

#include "src/lib/fxl/strings/string_printf.h"
#include "src/starnix/tests/selinux/userspace/util.h"

extern std::string DoPrePolicyLoadWork() { return "selinuxfs_policy.pp"; }

namespace {

using SeLinuxApiResult = fit::result<int, std::string>;

/// Calls the specified `api` with the supplied `request` and returns the resulting output.
SeLinuxApiResult CallSeLinuxApi(std::string_view api, std::string_view request) {
  constexpr char kSeLinuxFsApiFmt[] = "/sys/fs/selinux/%s";

  auto api_path = fxl::StringPrintf(kSeLinuxFsApiFmt, std::string(api).data());
  fbl::unique_fd api_fd(open(api_path.data(), O_RDWR));
  if (!api_fd.is_valid()) {
    return fit::error(errno);
  }

  if (!request.empty() && write(api_fd.get(), request.data(), request.length()) < 0) {
    return fit::error(errno);
  }

  std::string result;
  char read_buf[10];
  while (true) {
    ssize_t read_result = read(api_fd.get(), read_buf, sizeof(read_buf));
    if (read_result == 0) {
      // Some APIs include a trailing NUL under Linux, where Starnix does not include it.
      return fit::ok(RemoveTrailingNul(result));
    }
    if (read_result < 0) {
      return fit::error(errno);
    }
    result.append(read_buf, read_result);
  }
}

SeLinuxApiResult GetClassId(std::string_view class_name) {
  constexpr char kSeLinuxFsClassIndexApiNameFmt[] = "class/%s/index";

  auto api_name = fxl::StringPrintf(kSeLinuxFsClassIndexApiNameFmt, std::string(class_name).data());
  return CallSeLinuxApi(api_name, std::string_view());
}

SeLinuxApiResult ValidateContext(std::string_view context) {
  return CallSeLinuxApi("context", context);
}

SeLinuxApiResult ComputeCreateContext(std::string_view source_context,
                                      std::string_view target_context,
                                      std::string_view target_class) {
  auto class_id_string = GetClassId(target_class);
  if (class_id_string.is_error()) {
    return class_id_string;
  }
  auto class_id = atoi(class_id_string.value().data());

  constexpr char kSeLinuxCreateRequestFmt[] = "%s %s %hu";
  auto request = fxl::StringPrintf(kSeLinuxCreateRequestFmt, std::string(source_context).data(),
                                   std::string(target_context).data(), class_id);
  return CallSeLinuxApi("create", request);
}

TEST(SeLinuxFsContext, ValidatesRequiredFieldsPresent) {
  // Contexts that have too few colons to provide user, role, type & sensitivity are rejected.
  EXPECT_EQ(ValidateContext("test_selinuxfs_u"), fit::failed());
  EXPECT_EQ(ValidateContext("test_selinuxfs_u:test_selinuxfs_r"), fit::failed());
  EXPECT_EQ(ValidateContext("test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t"), fit::failed());

  // The minimum valid context has at least user, role, type and low/default sensitivity.
  constexpr std::string_view kMinimumValidContext =
      "test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0";
  EXPECT_EQ(ValidateContext(kMinimumValidContext), fit::ok(kMinimumValidContext));
}

TEST(SeLinuxFsContext, ValidatesFieldValues) {
  // Valid contexts are successfully written, and can be read-back.
  constexpr std::string_view kValidContext =
      "test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0:c0-s2:c0.c2";
  EXPECT_EQ(ValidateContext(kValidContext), fit::ok(kValidContext));

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
  // The "test_selinuxfs_u" user is granted the full range of categories.
  constexpr std::string_view kValidContext =
      "test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0:c0-s2:c0.c2";
  EXPECT_EQ(ValidateContext(kValidContext), fit::ok(kValidContext));

  // The "test_selinuxfs_limited_u" user is granted only "s0" sensitivity, must have "c0" category
  // and may have "c1" category.
  constexpr std::string_view kLimitedContext_Valid =
      "test_selinuxfs_limited_level_u:test_selinuxfs_r:test_selinuxfs_t:s0:c0";
  EXPECT_EQ(ValidateContext(kLimitedContext_Valid), fit::ok(kLimitedContext_Valid));

  constexpr std::string_view kLimitedContext_MissingCategory =
      "test_selinuxfs_limited_level_u:test_selinuxfs_r:test_selinuxfs_t:s0";
  EXPECT_EQ(ValidateContext(kLimitedContext_MissingCategory), fit::failed());

  constexpr std::string_view kLimitedContext_BadSensitivity =
      "test_selinuxfs_limited_level_u:test_selinuxfs_r:test_selinuxfs_t:s1:c0";
  EXPECT_EQ(ValidateContext(kLimitedContext_BadSensitivity), fit::failed());
}

TEST(SeLinuxFsContext, NormalizeCategories) {
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

// Validate that Security Contexts for new "process" class instances inherit the source role & type.
TEST(SeLinuxFsCreate, DefaultComputeCreateForProcess) {
  constexpr std::string_view kSourceContext =
      "test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0";
  constexpr std::string_view kTargetContext =
      "test_selinuxfs_create_target_u:test_selinuxfs_create_target_r:test_selinuxfs_create_target_t:s0";

  EXPECT_EQ(ComputeCreateContext(kSourceContext, kTargetContext, "process"),
            fit::ok("test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0"));
}

// Validate that Security Contexts for new socket-like class instances behave the same as "process".
TEST(SeLinuxFsCreate, DefaultComputeCreateForSocket) {
  constexpr std::string_view kSourceContext =
      "test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0";
  constexpr std::string_view kTargetContext =
      "test_selinuxfs_create_target_u:test_selinuxfs_create_target_r:test_selinuxfs_create_target_t:s0";

  EXPECT_EQ(ComputeCreateContext(kSourceContext, kTargetContext, "tcp_socket"),
            fit::ok("test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0"));
}

// Validate that Security Contexts for non-process/socket-like class instances receive "object_r"
// role, and the target/parent type.
TEST(SeLinuxFsCreate, DefaultComputeCreateForFile) {
  constexpr std::string_view kSourceContext =
      "test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0";
  constexpr std::string_view kTargetContext =
      "test_selinuxfs_create_target_u:test_selinuxfs_create_target_r:test_selinuxfs_create_target_t:s0";

  EXPECT_EQ(ComputeCreateContext(kSourceContext, kTargetContext, "file"),
            fit::ok("test_selinuxfs_u:object_r:test_selinuxfs_create_target_t:s0"));
}

// Validate handling of whitespace between request elements.
TEST(SeLinuxFsCreate, ExtraWhitespace) {
  constexpr std::string_view kExpectedContext =
      "test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0";

  // Additional whitespace between elements is ignored.
  EXPECT_EQ(
      ComputeCreateContext(" test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0",
                           "test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0", "process"),
      fit::ok(kExpectedContext));
  EXPECT_EQ(
      ComputeCreateContext("test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0 ",
                           "test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0", "process"),
      fit::ok(kExpectedContext));
  EXPECT_EQ(
      ComputeCreateContext("test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0",
                           "test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0 ", "process"),
      fit::ok(kExpectedContext));

  // Tabs and other whitespace are also valid separators.
  EXPECT_EQ(
      ComputeCreateContext("\ttest_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0",
                           "test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0", "process"),
      fit::ok(kExpectedContext));
  EXPECT_EQ(
      ComputeCreateContext("test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0\t",
                           "test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0", "process"),
      fit::ok(kExpectedContext));
  EXPECT_EQ(
      ComputeCreateContext("test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0",
                           "test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0\t", "process"),
      fit::ok(kExpectedContext));
  EXPECT_EQ(
      ComputeCreateContext("\ntest_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0",
                           "test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0\n", "process"),
      fit::ok(kExpectedContext));

  // Although whitespace around elements is ignored, only spaces are valid separators.
  EXPECT_EQ(
      CallSeLinuxApi(
          "create",
          "test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0\ttest_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0\tprocess"),
      fit::error(EINVAL));
  EXPECT_EQ(
      CallSeLinuxApi(
          "create",
          "test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0\ntest_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0\nprocess"),
      fit::error(EINVAL));
}

// Validate that various badly-formed "create" API requests are rejected as invalid.
TEST(SeLinuxFsCreate, MalformedComputeCreateRequest) {
  // Fewer than three arguments is an invalid request.
  EXPECT_EQ(CallSeLinuxApi("create", "test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0"),
            fit::error(EINVAL));
  EXPECT_EQ(
      CallSeLinuxApi(
          "create",
          "test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0 test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0"),
      fit::error(EINVAL));

  // More than four arguments is an invalid request.
  EXPECT_EQ(
      CallSeLinuxApi(
          "create",
          "test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0 test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0 process test bad"),
      fit::error(EINVAL));

  // If either security context is malformed, it is an invalid request.
  EXPECT_EQ(
      ComputeCreateContext("test_selinuxfs_non_existent_u:test_selinuxfs_r:test_selinuxfs_t:s0",
                           "test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0", "process"),
      fit::error(EINVAL));
  EXPECT_EQ(ComputeCreateContext(
                "test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0",
                "test_selinuxfs_non_existent_u:test_selinuxfs_r:test_selinuxfs_t:s0", "process"),
            fit::error(EINVAL));
}

// Validate handling of class Id values not defined in the loaded policy.
// It appears that invalid class Ids may be "clamped" to the class Id range, resulting in their
// being labeled according to whether the first/last class in the policy is process/socket-like, or
// not.
TEST(SeLinuxFsCreate, InvalidComputeCreateClassId) {
  // Zero is not a valid class Id, but is apparently treated as process/socket-like.
  EXPECT_EQ(
      CallSeLinuxApi(
          "create",
          "test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0 test_selinuxfs_create_target_u:test_selinuxfs_create_target_r:test_selinuxfs_create_target_t:s0 0"),
      fit::ok("test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0"));

  // 65535 is not a valid class Id in the test policy, but is apparently treated as
  // non-process/socket-like.
  EXPECT_EQ(
      CallSeLinuxApi(
          "create",
          "test_selinuxfs_u:test_selinuxfs_r:test_selinuxfs_t:s0 test_selinuxfs_create_target_u:test_selinuxfs_create_target_r:test_selinuxfs_create_target_t:s0 65535"),
      fit::ok("test_selinuxfs_u:object_r:test_selinuxfs_create_target_t:s0"));
}

}  // namespace
