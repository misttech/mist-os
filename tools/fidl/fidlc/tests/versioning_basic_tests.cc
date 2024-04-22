// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "tools/fidl/fidlc/src/versioning_types.h"
#include "tools/fidl/fidlc/tests/test_library.h"

// This file tests basic FIDL versioning behavior, such as elements being
// included in the output only between their `added` and `removed` versions.
// Tests are run for all the versions given in INSTANTIATE_TEST_SUITE_P.

namespace fidlc {
namespace {

class VersioningBasicTest : public testing::TestWithParam<Version> {};

const Version V1 = Version::From(1).value();
const Version V2 = Version::From(2).value();
const Version HEAD = Version::Head();
const Version LEGACY = Version::Legacy();

INSTANTIATE_TEST_SUITE_P(VersioningBasicTests, VersioningBasicTest,
                         testing::Values(V1, V2, HEAD, LEGACY),
                         [](auto info) { return info.param.ToString(); });

TEST_P(VersioningBasicTest, GoodLibraryDefault) {
  TestLibrary library(R"FIDL(
library example;
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
}

TEST_P(VersioningBasicTest, GoodLibraryAddedAtHead) {
  TestLibrary library(R"FIDL(
@available(added=HEAD)
library example;
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
}

TEST_P(VersioningBasicTest, GoodLibraryAddedAtOne) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
}

TEST_P(VersioningBasicTest, GoodLibraryAddedAndRemoved) {
  TestLibrary library(R"FIDL(
@available(added=1, removed=2)
library example;
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
}

TEST_P(VersioningBasicTest, GoodLibraryAddedAndDeprecatedAndRemoved) {
  TestLibrary library(R"FIDL(
@available(added=1, deprecated=2, removed=HEAD)
library example;
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
}

TEST_P(VersioningBasicTest, GoodLibraryAddedAndRemovedLegacyFalse) {
  TestLibrary library(R"FIDL(
@available(added=1, removed=2, legacy=false)
library example;
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
}

TEST_P(VersioningBasicTest, GoodLibraryAddedAndRemovedLegacyTrue) {
  TestLibrary library(R"FIDL(
@available(added=1, removed=2, legacy=true)
library example;
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
}

TEST_P(VersioningBasicTest, GoodDeclAddedAtHead) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=HEAD)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.HasStruct("Foo"), GetParam() >= HEAD);
}

TEST_P(VersioningBasicTest, GoodDeclAddedAtOne) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=1)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  EXPECT_TRUE(library.HasStruct("Foo"));
}

TEST_P(VersioningBasicTest, GoodDeclAddedAndRemoved) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=1, removed=2)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.HasStruct("Foo"), GetParam() >= V1 && GetParam() < V2);
}

TEST_P(VersioningBasicTest, GoodDeclAddedAndReplaced) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=1, replaced=2)
type Foo = struct {};

@available(added=2)
type Foo = resource struct {};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.LookupStruct("Foo")->resourceness,
            GetParam() == V1 ? Resourceness::kValue : Resourceness::kResource);
}

TEST_P(VersioningBasicTest, GoodDeclAddedAndDeprecatedAndRemoved) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=1, deprecated=2, removed=HEAD)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  ASSERT_EQ(library.HasStruct("Foo"), GetParam() < HEAD);
  if (GetParam() < HEAD) {
    EXPECT_EQ(library.LookupStruct("Foo")->availability.is_deprecated(), GetParam() >= V2);
  }
}

TEST_P(VersioningBasicTest, GoodDeclAddedAndRemovedLegacy) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=1, removed=2, legacy=true)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.HasStruct("Foo"), GetParam() == V1 || GetParam() == LEGACY);
}

TEST_P(VersioningBasicTest, GoodMemberAddedAtHead) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    @available(added=HEAD)
    member string;
};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.LookupStruct("Foo")->members.size(), GetParam() >= HEAD ? 1u : 0u);
}

TEST_P(VersioningBasicTest, GoodMemberAddedAtOne) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    @available(added=1)
    member string;
};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.LookupStruct("Foo")->members.size(), 1u);
}

TEST_P(VersioningBasicTest, GoodMemberAddedAndRemoved) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    @available(added=1, removed=2)
    member string;
};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.LookupStruct("Foo")->members.size(), GetParam() == V1 ? 1u : 0u);
}

TEST_P(VersioningBasicTest, GoodMemberAddedAndReplaced) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    @available(added=1, replaced=2)
    member string;
    @available(added=2)
    member uint32;
};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  ASSERT_EQ(library.LookupStruct("Foo")->members.size(), 1u);
  EXPECT_EQ(library.LookupStruct("Foo")->members.front().type_ctor->type->kind,
            GetParam() == V1 ? Type::Kind::kString : Type::Kind::kPrimitive);
}

TEST_P(VersioningBasicTest, GoodMemberAddedAndDeprecatedAndRemoved) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    @available(added=1, deprecated=2, removed=HEAD)
    member string;
};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  ASSERT_EQ(library.LookupStruct("Foo")->members.size(), GetParam() < HEAD ? 1u : 0u);
  if (GetParam() < HEAD) {
    EXPECT_EQ(library.LookupStruct("Foo")->members.front().availability.is_deprecated(),
              GetParam() >= V2);
  }
}

TEST_P(VersioningBasicTest, GoodMemberAddedAndRemovedLegacy) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    @available(added=1, removed=2, legacy=true)
    member string;
};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.LookupStruct("Foo")->members.size(), GetParam() == V1 || GetParam() == LEGACY);
}

// TODO(https://fxbug.dev/42052719): Generalize this with more comprehensive tests in
// versioning_interleaving_tests.cc.
TEST_P(VersioningBasicTest, GoodRegularDeprecatedReferencesVersionedDeprecated) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@deprecated
const FOO uint32 = BAR;
@available(deprecated=1)
const BAR uint32 = 1;
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
}

// Previously this errored due to incorrect logic in deprecation checks.
TEST_P(VersioningBasicTest, GoodDeprecationLogicRegression1) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(deprecated=1, removed=3)
type Foo = struct {};

@available(deprecated=1, removed=3)
type Bar = struct {
    foo Foo;
    @available(added=2)
    ensure_split_at_v2 string;
};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  ASSERT_EQ(library.HasStruct("Bar"), GetParam() <= V2);
  if (GetParam() <= V2) {
    EXPECT_EQ(library.LookupStruct("Bar")->members.size(), GetParam() == V1 ? 1u : 2u);
  }
}

// Previously this crashed due to incorrect logic in deprecation checks.
TEST_P(VersioningBasicTest, GoodDeprecationLogicRegression2) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(deprecated=1)
type Foo = struct {};

@available(deprecated=1, removed=3)
type Bar = struct {
    foo Foo;
    @available(added=2)
    ensure_split_at_v2 string;
};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  ASSERT_EQ(library.HasStruct("Bar"), GetParam() <= V2);
  if (GetParam() <= V2) {
    EXPECT_EQ(library.LookupStruct("Bar")->members.size(), GetParam() == V1 ? 1u : 2u);
  }
}

TEST_P(VersioningBasicTest, GoodMultipleFiles) {
  TestLibrary library;
  library.AddSource("overview.fidl", R"FIDL(
/// Some doc comment.
@available(added=1)
library example;
)FIDL");
  library.AddSource("first.fidl", R"FIDL(
library example;

@available(added=2)
type Foo = struct {
    bar box<Bar>;
};
)FIDL");
  library.AddSource("second.fidl", R"FIDL(
library example;

@available(added=2)
type Bar = struct {
    foo box<Foo>;
};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  ASSERT_EQ(library.HasStruct("Foo"), GetParam() >= V2);
  ASSERT_EQ(library.HasStruct("Bar"), GetParam() >= V2);
}

TEST_P(VersioningBasicTest, GoodMultipleLibraries) {
  SharedAmongstLibraries shared;
  shared.SelectVersion("platform", GetParam());
  TestLibrary dependency(&shared, "dependency.fidl", R"FIDL(
@available(added=1)
library platform.dependency;

type Foo = struct {
    @available(added=2)
    member string;
};
)FIDL");
  ASSERT_COMPILED(dependency);
  TestLibrary example(&shared, "example.fidl", R"FIDL(
@available(added=1)
library platform.example;

using platform.dependency;

type ShouldBeSplit = struct {
    foo platform.dependency.Foo;
};
)FIDL");
  ASSERT_COMPILED(example);
  ASSERT_EQ(example.LookupStruct("ShouldBeSplit")->type_shape->inline_size,
            GetParam() == V1 ? 1u : 16u);
}

}  // namespace
}  // namespace fidlc
