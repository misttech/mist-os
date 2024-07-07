// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/zx/result.h>
#include <zircon/errors.h>

#include <gtest/gtest-spi.h>
#include <gtest/gtest.h>

#include "src/lib/testing/predicates/status.h"

namespace {

class PredicatesTest : public ::testing::Test {
 public:
  // Declare some constants with zx_status_t values to assert on error messages.
  static constexpr zx_status_t kStatusOk = ZX_OK;
  static constexpr zx_status_t kStatusErrInternal = ZX_ERR_INTERNAL;
  static constexpr zx_status_t kStatusErrNotFound = ZX_ERR_NOT_FOUND;
};

TEST_F(PredicatesTest, CompareOk) {
  constexpr const char* kErrorMsg = "kStatusErrInternal is ZX_ERR_INTERNAL, expected ZX_OK.";
  // Test failure and error message.
  EXPECT_FATAL_FAILURE(ASSERT_OK(kStatusErrInternal), kErrorMsg);
  EXPECT_NONFATAL_FAILURE(EXPECT_OK(kStatusErrInternal), kErrorMsg);
  // Test success case.
  ASSERT_OK(kStatusOk);
  EXPECT_OK(kStatusOk);
}

TEST_F(PredicatesTest, CompareStatus) {
  constexpr const char* kErrorMsg =
      "Value of: kStatusErrNotFound\n  Actual: ZX_ERR_NOT_FOUND\nExpected: "
      "kStatusErrInternal\nWhich is: ZX_ERR_INTERNAL";
  // Test failure and error message.
  EXPECT_FATAL_FAILURE(ASSERT_STATUS(kStatusErrNotFound, kStatusErrInternal), kErrorMsg);
  EXPECT_NONFATAL_FAILURE(EXPECT_STATUS(kStatusErrNotFound, kStatusErrInternal), kErrorMsg);
  // Test success case.
  ASSERT_STATUS(kStatusErrInternal, ZX_ERR_INTERNAL);
  EXPECT_STATUS(kStatusErrInternal, ZX_ERR_INTERNAL);
}

TEST_F(PredicatesTest, CompareOkZxResult) {
  zx::result<int> ok_result = zx::ok(100);
  ASSERT_OK(ok_result);
  EXPECT_OK(ok_result);

  constexpr const char* kErrorMsg = "error_result is ZX_ERR_NOT_FOUND, expected ZX_OK.";
  EXPECT_FATAL_FAILURE(
      {
        zx::result<int> error_result = zx::error(ZX_ERR_NOT_FOUND);
        ASSERT_OK(error_result);
      },
      kErrorMsg);
  EXPECT_NONFATAL_FAILURE(
      {
        zx::result<int> error_result = zx::error(ZX_ERR_NOT_FOUND);
        EXPECT_OK(error_result);
      },
      kErrorMsg);
}

TEST_F(PredicatesTest, CompareOkZxResultWithNoValue) {
  zx::result<> ok_result = zx::ok();
  ASSERT_OK(ok_result);
  EXPECT_OK(ok_result);

  constexpr const char* kErrorMsg = "error_result is ZX_ERR_NOT_FOUND, expected ZX_OK.";
  EXPECT_FATAL_FAILURE(
      {
        zx::result<> error_result = zx::error(ZX_ERR_NOT_FOUND);
        ASSERT_OK(error_result);
      },
      kErrorMsg);
  EXPECT_NONFATAL_FAILURE(
      {
        zx::result<> error_result = zx::error(ZX_ERR_NOT_FOUND);
        EXPECT_OK(error_result);
      },
      kErrorMsg);
}

TEST_F(PredicatesTest, CompareStatusZxOkWithNoValue) {
  zx::result<> ok_result = zx::ok();
  ASSERT_STATUS(ok_result, zx::ok());
  ASSERT_STATUS(zx::ok(), ok_result);
  EXPECT_STATUS(ok_result, zx::ok());
  EXPECT_STATUS(zx::ok(), ok_result);

  constexpr const char* kErrorMsgOkResultComparedWithErrorResult =
      "Value of: ok_result\n"
      "  Actual: ZX_OK\n"
      "Expected: error_result\n"
      "Which is: ZX_ERR_INTERNAL";
  EXPECT_FATAL_FAILURE(
      {
        zx::result<> ok_result = zx::ok();
        zx::result<> error_result = zx::error(ZX_ERR_INTERNAL);
        ASSERT_STATUS(ok_result, error_result);
      },
      kErrorMsgOkResultComparedWithErrorResult);
  EXPECT_NONFATAL_FAILURE(
      {
        zx::result<> ok_result = zx::ok();
        zx::result<> error_result = zx::error(ZX_ERR_INTERNAL);
        EXPECT_STATUS(ok_result, error_result);
      },
      kErrorMsgOkResultComparedWithErrorResult);
}

TEST_F(PredicatesTest, CompareStatusZxErrorWithNoValue) {
  zx::result<> error_result = zx::error(ZX_ERR_INTERNAL);
  ASSERT_STATUS(error_result, zx::error(ZX_ERR_INTERNAL));
  EXPECT_STATUS(error_result, zx::error(ZX_ERR_INTERNAL));

  zx::result<> error_result2 = zx::error(ZX_ERR_INTERNAL);
  ASSERT_STATUS(error_result, error_result2);
  EXPECT_STATUS(error_result, error_result2);

  constexpr const char* kErrorMsgErrorResultComparedWithOkResult =
      "Value of: error_result\n"
      "  Actual: ZX_ERR_INTERNAL\n"
      "Expected: ok_result\n"
      "Which is: ZX_OK";
  EXPECT_FATAL_FAILURE(
      {
        zx::result<> error_result = zx::error(ZX_ERR_INTERNAL);
        zx::result<> ok_result = zx::ok();
        ASSERT_STATUS(error_result, ok_result);
      },
      kErrorMsgErrorResultComparedWithOkResult);
  EXPECT_NONFATAL_FAILURE(
      {
        zx::result<> error_result = zx::error(ZX_ERR_INTERNAL);
        zx::result<> ok_result = zx::ok();
        EXPECT_STATUS(error_result, ok_result);
      },
      kErrorMsgErrorResultComparedWithOkResult);

  constexpr const char* kErrorMsgErrorResultComparedWithErrorResult =
      "Value of: error_result1\n"
      "  Actual: ZX_ERR_INTERNAL\n"
      "Expected: error_result2\n"
      "Which is: ZX_ERR_NOT_FOUND";
  EXPECT_FATAL_FAILURE(
      {
        zx::result<> error_result1 = zx::error(ZX_ERR_INTERNAL);
        zx::result<> error_result2 = zx::error(ZX_ERR_NOT_FOUND);
        ASSERT_STATUS(error_result1, error_result2);
      },
      kErrorMsgErrorResultComparedWithErrorResult);
  EXPECT_NONFATAL_FAILURE(
      {
        zx::result<> error_result1 = zx::error(ZX_ERR_INTERNAL);
        zx::result<> error_result2 = zx::error(ZX_ERR_NOT_FOUND);
        EXPECT_STATUS(error_result1, error_result2);
      },
      kErrorMsgErrorResultComparedWithErrorResult);
}

TEST_F(PredicatesTest, CompareStatusZxOkWithValue) {
  zx::result<int> ok_result = zx::ok(100);
  ASSERT_STATUS(ok_result, zx::ok());
  ASSERT_STATUS(zx::ok(), ok_result);
  EXPECT_STATUS(ok_result, zx::ok());
  EXPECT_STATUS(zx::ok(), ok_result);

  constexpr const char* kErrorMsgOkResultComparedWithErrorLiteral =
      "Value of: ok_result\n"
      "  Actual: ZX_OK\n"
      "Expected: zx::error((-1))\n"
      "Which is: ZX_ERR_INTERNAL";
  EXPECT_FATAL_FAILURE(
      {
        zx::result<int> ok_result = zx::ok(100);
        ASSERT_STATUS(ok_result, zx::error(ZX_ERR_INTERNAL));
      },
      kErrorMsgOkResultComparedWithErrorLiteral);
  EXPECT_NONFATAL_FAILURE(
      {
        zx::result<int> ok_result = zx::ok(100);
        EXPECT_STATUS(ok_result, zx::error(ZX_ERR_INTERNAL));
      },
      kErrorMsgOkResultComparedWithErrorLiteral);

  constexpr const char* kErrorMsgErrorLiteralComparedWithOkResult =
      "Value of: zx::error((-1))\n"
      "  Actual: ZX_ERR_INTERNAL\n"
      "Expected: ok_result\n"
      "Which is: ZX_OK";
  EXPECT_FATAL_FAILURE(
      {
        zx::result<int> ok_result = zx::ok(100);
        ASSERT_STATUS(zx::error(ZX_ERR_INTERNAL), ok_result);
      },
      kErrorMsgErrorLiteralComparedWithOkResult);
  EXPECT_NONFATAL_FAILURE(
      {
        zx::result<int> ok_result = zx::ok(100);
        EXPECT_STATUS(zx::error(ZX_ERR_INTERNAL), ok_result);
      },
      kErrorMsgErrorLiteralComparedWithOkResult);
}

TEST_F(PredicatesTest, CompareStatusZxErrorWithValue) {
  zx::result<int> error_result = zx::error(ZX_ERR_INTERNAL);
  ASSERT_STATUS(error_result, zx::error(ZX_ERR_INTERNAL));
  EXPECT_STATUS(error_result, zx::error(ZX_ERR_INTERNAL));
  ASSERT_STATUS(zx::error(ZX_ERR_INTERNAL), error_result);
  EXPECT_STATUS(zx::error(ZX_ERR_INTERNAL), error_result);

  constexpr const char* kErrorMsgErrorResultComparedWithOkLiteral =
      "Value of: error_result\n"
      "  Actual: ZX_ERR_INTERNAL\n"
      "Expected: zx::ok()\n"
      "Which is: ZX_OK";
  EXPECT_FATAL_FAILURE(
      {
        zx::result<int> error_result = zx::error(ZX_ERR_INTERNAL);
        ASSERT_STATUS(error_result, zx::ok());
      },
      kErrorMsgErrorResultComparedWithOkLiteral);
  EXPECT_NONFATAL_FAILURE(
      {
        zx::result<int> error_result = zx::error(ZX_ERR_INTERNAL);
        EXPECT_STATUS(error_result, zx::ok());
      },
      kErrorMsgErrorResultComparedWithOkLiteral);

  constexpr const char* kErrorMsgOkLiteralComparedWithErrorResult =
      "Value of: zx::ok()\n"
      "  Actual: ZX_OK\n"
      "Expected: error_result\n"
      "Which is: ZX_ERR_INTERNAL";
  EXPECT_FATAL_FAILURE(
      {
        zx::result<int> error_result = zx::error(ZX_ERR_INTERNAL);
        ASSERT_STATUS(zx::ok(), error_result);
      },
      kErrorMsgOkLiteralComparedWithErrorResult);
  EXPECT_NONFATAL_FAILURE(
      {
        zx::result<int> error_result = zx::error(ZX_ERR_INTERNAL);
        EXPECT_STATUS(zx::ok(), error_result);
      },
      kErrorMsgOkLiteralComparedWithErrorResult);

  constexpr const char* kErrorMsgErrorResultComparedWithErrorLiteral =
      "Value of: error_result\n"
      "  Actual: ZX_ERR_INTERNAL\n"
      "Expected: zx::error((-25))\n"
      "Which is: ZX_ERR_NOT_FOUND";
  EXPECT_FATAL_FAILURE(
      {
        zx::result<int> error_result = zx::error(ZX_ERR_INTERNAL);
        ASSERT_STATUS(error_result, zx::error(ZX_ERR_NOT_FOUND));
      },
      kErrorMsgErrorResultComparedWithErrorLiteral);
  EXPECT_NONFATAL_FAILURE(
      {
        zx::result<int> error_result = zx::error(ZX_ERR_INTERNAL);
        EXPECT_STATUS(error_result, zx::error(ZX_ERR_NOT_FOUND));
      },
      kErrorMsgErrorResultComparedWithErrorLiteral);

  constexpr const char* kErrorMsgErrorLiteralComparedWithErrorResult =
      "Value of: zx::error((-25))\n"
      "  Actual: ZX_ERR_NOT_FOUND\n"
      "Expected: error_result\n"
      "Which is: ZX_ERR_INTERNAL";
  EXPECT_FATAL_FAILURE(
      {
        zx::result<int> error_result = zx::error(ZX_ERR_INTERNAL);
        ASSERT_STATUS(zx::error(ZX_ERR_NOT_FOUND), error_result);
      },
      kErrorMsgErrorLiteralComparedWithErrorResult);
  EXPECT_NONFATAL_FAILURE(
      {
        zx::result<int> error_result = zx::error(ZX_ERR_INTERNAL);
        EXPECT_STATUS(zx::error(ZX_ERR_NOT_FOUND), error_result);
      },
      kErrorMsgErrorLiteralComparedWithErrorResult);
}

}  // namespace
