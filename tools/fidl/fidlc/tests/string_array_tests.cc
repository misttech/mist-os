// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "tools/fidl/fidlc/tests/test_library.h"

namespace {

const char* good_library_source = R"FIDL(library example;

type S = struct {
    arr string_array<10>;
};
)FIDL";

TEST(StringArrayTests, GoodNonzeroSizeArray) {
  TestLibrary library(good_library_source);
  library.EnableFlag(fidlc::ExperimentalFlags::Flag::kZxCTypes);
  ASSERT_COMPILED(library);
}

TEST(StringArrayTests, BadNoExperimentalFlag) {
  TestLibrary library(good_library_source);
  library.ExpectFail(fidlc::ErrExperimentalZxCTypesDisallowed, "string_array");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(StringArrayTests, BadZeroSizeArray) {
  TestLibrary library(R"FIDL(library example;

type S = struct {
    arr string_array<0>;
};
)FIDL");
  library.EnableFlag(fidlc::ExperimentalFlags::Flag::kZxCTypes);
  library.ExpectFail(fidlc::ErrMustHaveNonZeroSize, "string_array");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(StringArrayTests, BadNoSizeArray) {
  TestLibrary library(R"FIDL(
library example;

type S = struct {
    arr string_array;
};
)FIDL");
  library.EnableFlag(fidlc::ExperimentalFlags::Flag::kZxCTypes);
  library.ExpectFail(fidlc::ErrWrongNumberOfLayoutParameters, "string_array", 1, 0);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(StringArrayTests, BadOptionalArray) {
  TestLibrary library(R"FIDL(
library example;

type S = struct {
    arr string_array<10>:optional;
};
)FIDL");
  library.EnableFlag(fidlc::ExperimentalFlags::Flag::kZxCTypes);
  library.ExpectFail(fidlc::ErrCannotBeOptional, "string_array");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

}  // namespace
