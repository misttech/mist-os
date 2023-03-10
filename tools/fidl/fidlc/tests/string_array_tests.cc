// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zxtest/zxtest.h>

#include "tools/fidl/fidlc/tests/error_test.h"
#include "tools/fidl/fidlc/tests/test_library.h"

namespace {

const char* good_library_source = R"FIDL(library example;

type S = struct {
    arr string_array<10>;
};
)FIDL";

TEST(StringArrayTests, GoodNonzeroSizeArray) {
  TestLibrary library(good_library_source);
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kZxCTypes);
  ASSERT_COMPILED(library);
}

TEST(StringArrayTests, BadNoExperimentalFlag) {
  TestLibrary library(good_library_source);
  ASSERT_ERRORED_DURING_COMPILE(library, fidl::ErrExperimentalZxCTypesDisallowed);
}

TEST(StringArrayTests, BadZeroSizeArray) {
  TestLibrary library(R"FIDL(library example;

type S = struct {
    arr string_array<0>;
};
)FIDL");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kZxCTypes);
  ASSERT_ERRORED_DURING_COMPILE(library, fidl::ErrMustHaveNonZeroSize);
}

TEST(StringArrayTests, BadNoSizeArray) {
  TestLibrary library(R"FIDL(
library example;

type S = struct {
    arr string_array;
};
)FIDL");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kZxCTypes);
  ASSERT_ERRORED_DURING_COMPILE(library, fidl::ErrWrongNumberOfLayoutParameters);
}

TEST(StringArrayTests, BadOptionalArray) {
  TestLibrary library(R"FIDL(
library example;

type S = struct {
    arr string_array<10>:optional;
};
)FIDL");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kZxCTypes);
  ASSERT_ERRORED_DURING_COMPILE(library, fidl::ErrCannotBeOptional);
}

}  // namespace
