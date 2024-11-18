// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/util/bstring.h>
#include <lib/mistos/util/testing/unittest.h>
#include <lib/unittest/unittest.h>

namespace unit_testing {
namespace {

using starnix_uapi::Errno;
using starnix_uapi::ErrnoCode;

bool basic_errno_formatting() {
  BEGIN_TEST;

  auto location = std::source_location::current();
  auto errno = Errno::New(ErrnoCode(ENOENT, STRINGIFY_IMPL(ENOENT)), location);

  auto location_str = starnix_uapi::to_string(location);
  auto format = mtl::format("errno ENOENT(2) from %.*s", static_cast<int>(location_str.size()),
                            location_str.data());
  ASSERT_STREQ(format, errno.to_string());

  END_TEST;
}

bool context_errno_formatting() {
  BEGIN_TEST;

  auto location = std::source_location::current();
  auto errno =
      Errno::with_context(ErrnoCode(ENOENT, STRINGIFY_IMPL(ENOENT)), location, "TEST CONTEXT");
  auto location_str = starnix_uapi::to_string(location);
  auto format = mtl::format("errno ENOENT(2) from %.*s, context: TEST CONTEXT",
                            static_cast<int>(location_str.size()), location_str.data());
  ASSERT_STREQ(format, errno.to_string());

  END_TEST;
}

bool with_source_context() {
  BEGIN_TEST;

  auto errno =
      Errno::New(ErrnoCode(ENOENT, STRINGIFY_IMPL(ENOENT)), std::source_location::current());
  fit::result<Errno> result = fit::error(errno);
  auto error = starnix_uapi::make_source_context(result)
                   .with_source_context([]() { return "42"; })
                   .error_value();
  auto line_after_error = std::source_location::current();

  // the -3 offset must match the with_source_context line
  auto expected_prefix =
      mtl::format("42, %s:%d:", line_after_error.file_name(), line_after_error.line() - 3);

  auto error_str = error.to_string();
  ASSERT_BYTES_EQ((const uint8_t*)(expected_prefix.data()), (const uint8_t*)(error_str.data()),
                  expected_prefix.size());

  END_TEST;
}

bool source_context() {
  BEGIN_TEST;

  auto errno =
      Errno::New(ErrnoCode(ENOENT, STRINGIFY_IMPL(ENOENT)), std::source_location::current());
  fit::result<Errno> result = fit::error(errno);
  auto error = starnix_uapi::make_source_context(result).source_context("42").error_value();
  auto line_after_error = std::source_location::current();
  auto expected_prefix =
      mtl::format("42, %s:%d:", line_after_error.file_name(), line_after_error.line() - 1);

  auto error_str = error.to_string();
  ASSERT_BYTES_EQ((const uint8_t*)(expected_prefix.data()), (const uint8_t*)(error_str.data()),
                  expected_prefix.size());

  END_TEST;
}

}  // namespace
}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_uapi_errors)
UNITTEST("basic errno formatting", unit_testing::basic_errno_formatting)
UNITTEST("context errno formatting", unit_testing::context_errno_formatting)
UNITTEST("with source context", unit_testing::with_source_context)
UNITTEST("source context", unit_testing::source_context)
UNITTEST_END_TESTCASE(starnix_uapi_errors, "starnix_uapi_errors", "Tests Errors")
