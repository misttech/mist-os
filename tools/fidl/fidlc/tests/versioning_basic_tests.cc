// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "tools/fidl/fidlc/src/versioning_types.h"
#include "tools/fidl/fidlc/tests/test_library.h"

// This file tests basic FIDL versioning behavior and edge cases that don't
// belong in any of the other versioning_*_tests.cc files.

namespace fidlc {
namespace {

// Largest numeric version accepted by Version::Parse.
const std::string kMaxNumericVersion = std::to_string((1ull << 63) - 1);

TEST(VersioningTests, GoodLibraryDefault) {
  auto source = R"FIDL(
library example;
)FIDL";

  for (auto version : {"1", "2", kMaxNumericVersion.c_str(), "HEAD", "LEGACY"}) {
    TestLibrary library(source);
    library.SelectVersion("example", version);
    ASSERT_COMPILED(library);
  }
}

TEST(VersioningTests, GoodLibraryAddedAtHead) {
  auto source = R"FIDL(
@available(added=HEAD)
library example;
)FIDL";

  for (auto version : {"1", "2", kMaxNumericVersion.c_str(), "HEAD", "LEGACY"}) {
    TestLibrary library(source);
    library.SelectVersion("example", version);
    ASSERT_COMPILED(library);
  }
}

TEST(VersioningTests, GoodLibraryAddedAtOne) {
  auto source = R"FIDL(
@available(added=1)
library example;
)FIDL";

  for (auto version : {"1", "2", kMaxNumericVersion.c_str(), "HEAD", "LEGACY"}) {
    TestLibrary library(source);
    library.SelectVersion("example", version);
    ASSERT_COMPILED(library);
  }
}

TEST(VersioningTests, GoodLibraryAddedAndRemoved) {
  auto source = R"FIDL(
@available(added=1, removed=2)
library example;
)FIDL";

  for (auto version : {"1", "2", kMaxNumericVersion.c_str(), "HEAD", "LEGACY"}) {
    TestLibrary library(source);
    library.SelectVersion("example", version);
    ASSERT_COMPILED(library);
  }
}

TEST(VersioningTests, GoodLibraryAddedAndDeprecatedAndRemoved) {
  auto source = R"FIDL(
@available(added=1, deprecated=2, removed=HEAD)
library example;
)FIDL";

  for (auto version : {"1", "2", kMaxNumericVersion.c_str(), "HEAD", "LEGACY"}) {
    TestLibrary library(source);
    library.SelectVersion("example", version);
    ASSERT_COMPILED(library);
  }
}

TEST(VersioningTests, GoodLibraryAddedAndRemovedLegacyFalse) {
  auto source = R"FIDL(
@available(added=1, removed=2, legacy=false)
library example;
)FIDL";

  for (auto version : {"1", "2", kMaxNumericVersion.c_str(), "HEAD", "LEGACY"}) {
    TestLibrary library(source);
    library.SelectVersion("example", version);
    ASSERT_COMPILED(library);
  }
}

TEST(VersioningTests, GoodLibraryAddedAndRemovedLegacyTrue) {
  auto source = R"FIDL(
@available(added=1, removed=2, legacy=true)
library example;
)FIDL";

  for (auto version : {"1", "2", kMaxNumericVersion.c_str(), "HEAD", "LEGACY"}) {
    TestLibrary library(source);
    library.SelectVersion("example", version);
    ASSERT_COMPILED(library);
  }
}

TEST(VersioningTests, GoodDeclAddedAtHead) {
  auto source = R"FIDL(
@available(added=1)
library example;

@available(added=HEAD)
type Foo = struct {};
)FIDL";

  {
    TestLibrary library(source);
    library.SelectVersion("example", "1");
    ASSERT_COMPILED(library);
    ASSERT_EQ(library.LookupStruct("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "2");
    ASSERT_COMPILED(library);
    ASSERT_EQ(library.LookupStruct("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", kMaxNumericVersion);
    ASSERT_COMPILED(library);
    ASSERT_EQ(library.LookupStruct("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "HEAD");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "LEGACY");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
  }
}

TEST(VersioningTests, GoodDeclAddedAtOne) {
  auto source = R"FIDL(
@available(added=1)
library example;

@available(added=1)
type Foo = struct {};
)FIDL";

  {
    TestLibrary library(source);
    library.SelectVersion("example", "1");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "2");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", kMaxNumericVersion);
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "HEAD");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "LEGACY");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
  }
}

TEST(VersioningTests, GoodDeclAddedAndRemoved) {
  auto source = R"FIDL(
@available(added=1)
library example;

@available(added=1, removed=2)
type Foo = struct {};
)FIDL";

  {
    TestLibrary library(source);
    library.SelectVersion("example", "1");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "2");
    ASSERT_COMPILED(library);
    ASSERT_EQ(library.LookupStruct("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", kMaxNumericVersion);
    ASSERT_COMPILED(library);
    ASSERT_EQ(library.LookupStruct("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "HEAD");
    ASSERT_COMPILED(library);
    ASSERT_EQ(library.LookupStruct("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "LEGACY");
    ASSERT_COMPILED(library);
    ASSERT_EQ(library.LookupStruct("Foo"), nullptr);
  }
}

TEST(VersioningTests, GoodDeclAddedAndReplaced) {
  auto source = R"FIDL(
@available(added=1)
library example;

@available(added=1, replaced=2)
type Foo = struct {};

@available(added=2)
type Foo = table {};
)FIDL";

  {
    TestLibrary library(source);
    library.SelectVersion("example", "1");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "2");
    ASSERT_COMPILED(library);
    ASSERT_EQ(library.LookupStruct("Foo"), nullptr);
    ASSERT_NE(library.LookupTable("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", kMaxNumericVersion);
    ASSERT_COMPILED(library);
    ASSERT_EQ(library.LookupStruct("Foo"), nullptr);
    ASSERT_NE(library.LookupTable("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "HEAD");
    ASSERT_COMPILED(library);
    ASSERT_EQ(library.LookupStruct("Foo"), nullptr);
    ASSERT_NE(library.LookupTable("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "LEGACY");
    ASSERT_COMPILED(library);
    ASSERT_EQ(library.LookupStruct("Foo"), nullptr);
    ASSERT_NE(library.LookupTable("Foo"), nullptr);
  }
}

TEST(VersioningTests, GoodDeclAddedAndDeprecatedAndRemoved) {
  auto source = R"FIDL(
@available(added=1)
library example;

@available(added=1, deprecated=2, removed=HEAD)
type Foo = struct {};
)FIDL";

  {
    TestLibrary library(source);
    library.SelectVersion("example", "1");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    EXPECT_FALSE(library.LookupStruct("Foo")->availability.is_deprecated());
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "2");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    EXPECT_TRUE(library.LookupStruct("Foo")->availability.is_deprecated());
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", kMaxNumericVersion);
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    EXPECT_TRUE(library.LookupStruct("Foo")->availability.is_deprecated());
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "HEAD");
    ASSERT_COMPILED(library);
    ASSERT_EQ(library.LookupStruct("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "LEGACY");
    ASSERT_COMPILED(library);
    ASSERT_EQ(library.LookupStruct("Foo"), nullptr);
  }
}

TEST(VersioningTests, GoodDeclAddedAndRemovedLegacy) {
  auto source = R"FIDL(
@available(added=1)
library example;

@available(added=1, removed=2, legacy=true)
type Foo = struct {};
)FIDL";

  {
    TestLibrary library(source);
    library.SelectVersion("example", "1");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "2");
    ASSERT_COMPILED(library);
    ASSERT_EQ(library.LookupStruct("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", kMaxNumericVersion);
    ASSERT_COMPILED(library);
    ASSERT_EQ(library.LookupStruct("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "HEAD");
    ASSERT_COMPILED(library);
    ASSERT_EQ(library.LookupStruct("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "LEGACY");
    ASSERT_COMPILED(library);
    // The decl is re-added at LEGACY.
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
  }
}

TEST(VersioningTests, GoodMemberAddedAtHead) {
  auto source = R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    @available(added=HEAD)
    member string;
};
)FIDL";

  {
    TestLibrary library(source);
    library.SelectVersion("example", "1");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    ASSERT_EQ(library.LookupStruct("Foo")->members.size(), 0u);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "2");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    ASSERT_EQ(library.LookupStruct("Foo")->members.size(), 0u);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", kMaxNumericVersion);
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    ASSERT_EQ(library.LookupStruct("Foo")->members.size(), 0u);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "HEAD");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    ASSERT_EQ(library.LookupStruct("Foo")->members.size(), 1u);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "LEGACY");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    ASSERT_EQ(library.LookupStruct("Foo")->members.size(), 1u);
  }
}

TEST(VersioningTests, GoodMemberAddedAtOne) {
  auto source = R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    @available(added=1)
    member string;
};
)FIDL";

  {
    TestLibrary library(source);
    library.SelectVersion("example", "1");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    ASSERT_EQ(library.LookupStruct("Foo")->members.size(), 1u);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "2");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    ASSERT_EQ(library.LookupStruct("Foo")->members.size(), 1u);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", kMaxNumericVersion);
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    ASSERT_EQ(library.LookupStruct("Foo")->members.size(), 1u);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "HEAD");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    ASSERT_EQ(library.LookupStruct("Foo")->members.size(), 1u);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "LEGACY");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    ASSERT_EQ(library.LookupStruct("Foo")->members.size(), 1u);
  }
}

TEST(VersioningTests, GoodMemberAddedAndRemoved) {
  auto source = R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    @available(added=1, removed=2)
    member string;
};
)FIDL";

  {
    TestLibrary library(source);
    library.SelectVersion("example", "1");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    ASSERT_EQ(library.LookupStruct("Foo")->members.size(), 1u);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "2");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    ASSERT_EQ(library.LookupStruct("Foo")->members.size(), 0u);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", kMaxNumericVersion);
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    ASSERT_EQ(library.LookupStruct("Foo")->members.size(), 0u);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "HEAD");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    ASSERT_EQ(library.LookupStruct("Foo")->members.size(), 0u);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "LEGACY");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    ASSERT_EQ(library.LookupStruct("Foo")->members.size(), 0u);
  }
}

TEST(VersioningTests, GoodMemberAddedAndReplaced) {
  auto source = R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    @available(added=1, replaced=2)
    member string;
    @available(added=2)
    member uint32;
};
)FIDL";

  {
    TestLibrary library(source);
    library.SelectVersion("example", "1");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    ASSERT_EQ(library.LookupStruct("Foo")->members.size(), 1u);
    ASSERT_EQ(library.LookupStruct("Foo")->members.front().type_ctor->type->kind,
              Type::Kind::kString);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "2");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    ASSERT_EQ(library.LookupStruct("Foo")->members.size(), 1u);
    ASSERT_EQ(library.LookupStruct("Foo")->members.front().type_ctor->type->kind,
              Type::Kind::kPrimitive);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", kMaxNumericVersion);
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    ASSERT_EQ(library.LookupStruct("Foo")->members.size(), 1u);
    ASSERT_EQ(library.LookupStruct("Foo")->members.front().type_ctor->type->kind,
              Type::Kind::kPrimitive);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "HEAD");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    ASSERT_EQ(library.LookupStruct("Foo")->members.size(), 1u);
    ASSERT_EQ(library.LookupStruct("Foo")->members.front().type_ctor->type->kind,
              Type::Kind::kPrimitive);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "LEGACY");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    ASSERT_EQ(library.LookupStruct("Foo")->members.size(), 1u);
    ASSERT_EQ(library.LookupStruct("Foo")->members.front().type_ctor->type->kind,
              Type::Kind::kPrimitive);
  }
}

TEST(VersioningTests, GoodMemberAddedAndDeprecatedAndRemoved) {
  auto source = R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    @available(added=1, deprecated=2, removed=HEAD)
    member string;
};
)FIDL";

  {
    TestLibrary library(source);
    library.SelectVersion("example", "1");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    ASSERT_EQ(library.LookupStruct("Foo")->members.size(), 1u);
    EXPECT_FALSE(library.LookupStruct("Foo")->members.front().availability.is_deprecated());
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "2");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    ASSERT_EQ(library.LookupStruct("Foo")->members.size(), 1u);
    EXPECT_TRUE(library.LookupStruct("Foo")->members.front().availability.is_deprecated());
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", kMaxNumericVersion);
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    ASSERT_EQ(library.LookupStruct("Foo")->members.size(), 1u);
    EXPECT_TRUE(library.LookupStruct("Foo")->members.front().availability.is_deprecated());
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "HEAD");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    ASSERT_EQ(library.LookupStruct("Foo")->members.size(), 0u);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "LEGACY");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    ASSERT_EQ(library.LookupStruct("Foo")->members.size(), 0u);
  }
}

TEST(VersioningTests, GoodMemberAddedAndRemovedLegacy) {
  auto source = R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    @available(added=1, removed=2, legacy=true)
    member string;
};
)FIDL";

  {
    TestLibrary library(source);
    library.SelectVersion("example", "1");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    ASSERT_EQ(library.LookupStruct("Foo")->members.size(), 1u);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "2");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    ASSERT_EQ(library.LookupStruct("Foo")->members.size(), 0u);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", kMaxNumericVersion);
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    ASSERT_EQ(library.LookupStruct("Foo")->members.size(), 0u);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "HEAD");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    ASSERT_EQ(library.LookupStruct("Foo")->members.size(), 0u);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "LEGACY");
    ASSERT_COMPILED(library);
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
    // The member is re-added at LEGACY.
    ASSERT_EQ(library.LookupStruct("Foo")->members.size(), 1u);
  }
}

// TODO(https://fxbug.dev/42052719): Generalize this with more comprehensive tests in
// versioning_interleaving_tests.cc.
TEST(VersioningTests, GoodRegularDeprecatedReferencesVersionedDeprecated) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@deprecated
const FOO uint32 = BAR;
@available(deprecated=1)
const BAR uint32 = 1;
)FIDL");
  library.SelectVersion("example", "HEAD");
  ASSERT_COMPILED(library);
}

// Previously this errored due to incorrect logic in deprecation checks.
TEST(VersioningTests, GoodDeprecationLogicRegression1) {
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
  library.SelectVersion("example", "HEAD");
  ASSERT_COMPILED(library);
}

// Previously this crashed due to incorrect logic in deprecation checks.
TEST(VersioningTests, BadDeprecationLogicRegression2) {
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
  library.SelectVersion("example", "HEAD");
  ASSERT_COMPILED(library);
}

TEST(VersioningTests, GoodMultipleFiles) {
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
  library.SelectVersion("example", "HEAD");
  ASSERT_COMPILED(library);
  ASSERT_NE(library.LookupStruct("Foo"), nullptr);
  ASSERT_NE(library.LookupStruct("Bar"), nullptr);
}

TEST(VersioningTests, GoodSplitByDeclInExternalLibrary) {
  SharedAmongstLibraries shared;
  shared.SelectVersion("platform", "HEAD");

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
}

}  // namespace
}  // namespace fidlc
