// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "tools/fidl/fidlc/tests/test_library.h"

// This file tests how FIDL versioning distinguishes "removal" from
// "replacement", and validation that replacement is done correctly.
// Tests are run for all the versions given in INSTANTIATE_TEST_SUITE_P.

namespace fidlc {
namespace {

class VersioningReplacementTest : public testing::TestWithParam<Version> {};

const Version V1 = Version::From(1).value();
const Version V2 = Version::From(2).value();
const Version V3 = Version::From(3).value();

INSTANTIATE_TEST_SUITE_P(VersioningReplacementTests, VersioningReplacementTest,
                         testing::Values(V1, V2, V3),
                         [](auto info) { return info.param.ToString(); });

TEST_P(VersioningReplacementTest, BadRemovedWithReplacement) {
  TestLibrary library;
  library.SelectVersion("test", GetParam());
  library.AddFile("bad/fi-0205.test.fidl");
  library.ExpectFail(ErrRemovedWithReplacement, "Bar", Version::From(2).value(),
                     "bad/fi-0205.test.fidl:11:14");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningReplacementTest, BadRemovedNamedToAnonymous) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(removed=2)
type Foo = struct {};

type Bar = struct {
    @available(added=2)
    foo struct {};
};
)FIDL");
  library.SelectVersion("example", GetParam());
  library.ExpectFail(ErrRemovedWithReplacement, "Foo", Version::From(2).value(),
                     "example.fidl:10:9");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningReplacementTest, GoodRemovedAnonymousToNamed) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Bar = struct {
    // The anonymous type "Foo" inherits removed=2, but removed/replaced
    // does not apply to inherited availabilities.
    @available(removed=2)
    foo struct {};
};

@available(added=2)
type Foo = table {};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.HasStruct("Foo"), GetParam() == V1);
  EXPECT_EQ(library.HasTable("Foo"), GetParam() >= V2);
}

TEST_P(VersioningReplacementTest, GoodRemovedAnonymousToAnonymous) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Bar1 = struct {
    // The anonymous type "Foo" inherits removed=2, but removed/replaced
    // does not apply to inherited availabilities.
    @available(removed=2)
    foo struct {};
};

type Bar2 = struct {
    @available(added=2)
    foo table {};
};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.HasStruct("Foo"), GetParam() == V1);
  EXPECT_EQ(library.HasTable("Foo"), GetParam() >= V2);
}

TEST_P(VersioningReplacementTest, BadReplacedWithoutReplacement) {
  TestLibrary library;
  library.SelectVersion("test", GetParam());
  library.AddFile("bad/fi-0206.test.fidl");
  library.ExpectFail(ErrReplacedWithoutReplacement, "Bar", Version::From(2).value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningReplacementTest, GoodAnonymousReplacedWithoutReplacement) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Bar = struct {
    // The anonymous type "Foo" inherits replaced=2, but removed/replaced
    // validation does not apply to inherited availabilities.
    @available(replaced=2)
    foo struct {};
    @available(added=2)
    foo string;
};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.HasStruct("Foo"), GetParam() == V1);
}

TEST_P(VersioningReplacementTest, GoodReplacedNamedToAnonymous) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(replaced=2)
type Foo = struct {};

type Bar = struct {
    @available(added=2)
    foo table {};
};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.HasStruct("Foo"), GetParam() == V1);
  EXPECT_EQ(library.HasTable("Foo"), GetParam() >= V2);
}

TEST_P(VersioningReplacementTest, GoodReplacedAnonymousToNamed) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Bar = struct {
    @available(replaced=2)
    foo struct {};
    @available(added=2)
    foo string;
};

@available(added=2)
type Foo = table {};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.HasStruct("Foo"), GetParam() == V1);
  EXPECT_EQ(library.HasTable("Foo"), GetParam() >= V2);
}

TEST_P(VersioningReplacementTest, GoodReplacedAnonymousToAnonymous) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Bar1 = struct {
    @available(replaced=2)
    foo struct {};
    @available(added=2)
    foo string;
};

type Bar2 = struct {
    @available(added=2)
    foo table {};
};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.HasStruct("Foo"), GetParam() == V1);
  EXPECT_EQ(library.HasTable("Foo"), GetParam() >= V2);
}

TEST_P(VersioningReplacementTest, GoodReplacedTwice) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(replaced=2)
type Foo = struct {};

@available(added=2, replaced=3)
type Foo = table {};

@available(added=3)
type Foo = union {};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.HasStruct("Foo"), GetParam() == V1);
  EXPECT_EQ(library.HasTable("Foo"), GetParam() == V2);
  EXPECT_EQ(library.HasUnion("Foo"), GetParam() >= V3);
}

}  // namespace
}  // namespace fidlc
