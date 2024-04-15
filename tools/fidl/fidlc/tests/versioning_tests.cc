// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <optional>

#include <gtest/gtest.h>

#include "tools/fidl/fidlc/src/diagnostics.h"
#include "tools/fidl/fidlc/src/versioning_types.h"
#include "tools/fidl/fidlc/tests/test_library.h"

// This file tests the behavior of the @available attribute. See also
// decomposition_tests.cc and availability_interleaving_tests.cc.

namespace fidlc {
namespace {

// Largest numeric version accepted by Version::Parse.
const std::string kMaxNumericVersion = std::to_string((1ull << 63) - 1);

TEST(VersioningTests, GoodUnversionedPlatformOneComponent) {
  TestLibrary library(R"FIDL(
library example;
)FIDL");
  ASSERT_COMPILED(library);
  EXPECT_TRUE(library.platform().is_unversioned());
}

TEST(VersioningTests, GoodUnversionedPlatformTwoComponents) {
  TestLibrary library(R"FIDL(
library example.something;
)FIDL");
  ASSERT_COMPILED(library);
  EXPECT_TRUE(library.platform().is_unversioned());
}

TEST(VersioningTests, GoodImplicitPlatformOneComponent) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.platform(), Platform::Parse("example").value());
  EXPECT_FALSE(library.platform().is_unversioned());
}

TEST(VersioningTests, GoodImplicitPlatformTwoComponents) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example.something;
)FIDL");
  library.SelectVersion("example", "HEAD");
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.platform(), Platform::Parse("example").value());
  EXPECT_FALSE(library.platform().is_unversioned());
}

TEST(VersioningTests, GoodExplicitPlatform) {
  TestLibrary library(R"FIDL(
@available(platform="someplatform", added=HEAD)
library example;
)FIDL");
  library.SelectVersion("someplatform", "HEAD");
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.platform(), Platform::Parse("someplatform").value());
  EXPECT_FALSE(library.platform().is_unversioned());
}

TEST(VersioningTests, BadInvalidPlatform) {
  TestLibrary library;
  library.AddFile("bad/fi-0152.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(ErrInvalidPlatform, "Spaces are not allowed");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadReservedPlatform) {
  TestLibrary library;
  library.AddFile("bad/fi-0208.test.fidl");
  library.ExpectFail(ErrReservedPlatform, "unversioned");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadExplicitPlatformNoVersionSelected) {
  TestLibrary library;
  library.AddFile("bad/fi-0201.test.fidl");
  library.ExpectFail(ErrPlatformVersionNotSelected, "library 'test.bad.fi0201'",
                     Platform::Parse("foo").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadImplicitPlatformNoVersionSelected) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example.something;
)FIDL");
  library.ExpectFail(ErrPlatformVersionNotSelected, "library 'example.something'",
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadAttributeOnMultipleLibraryDeclarationsAgree) {
  TestLibrary library;
  library.AddSource("first.fidl", R"FIDL(
@available(added=1)
library example;
)FIDL");
  library.AddSource("second.fidl", R"FIDL(
@available(added=1)
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrDuplicateAttribute, "available", "first.fidl:2:2");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadAttributeOnMultipleLibraryDeclarationsDisagree) {
  TestLibrary library;
  library.AddSource("first.fidl", R"FIDL(
@available(added=1)
library example;
)FIDL");
  library.AddSource("second.fidl", R"FIDL(
@available(added=2)
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrDuplicateAttribute, "available", "first.fidl:2:2");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadAttributeOnMultipleLibraryDeclarationsHead) {
  TestLibrary library;
  library.AddSource("first.fidl", R"FIDL(
@available(added=HEAD)
library example;
)FIDL");
  library.AddSource("second.fidl", R"FIDL(
@available(added=HEAD)
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  // TODO(https://fxbug.dev/42062904): Check for duplicate attributes earlier in
  // compilation so that this is ErrDuplicateAttribute instead.
  library.ExpectFail(ErrReferenceInLibraryAttribute);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

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

  {
    TestLibrary library(source);
    library.SelectVersion("example", "1");
    ASSERT_COMPILED(library);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "2");
    ASSERT_COMPILED(library);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", kMaxNumericVersion);
    ASSERT_COMPILED(library);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "HEAD");
    ASSERT_COMPILED(library);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "LEGACY");
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

TEST(VersioningTests, GoodAllArgumentsOnLibrary) {
  TestLibrary library(R"FIDL(
@available(platform="notexample", added=1, deprecated=2, removed=3, note="use xyz instead", legacy=false)
library example;
)FIDL");
  library.SelectVersion("notexample", "1");
  ASSERT_COMPILED(library);
}

TEST(VersioningTests, GoodAllArgumentsOnDecl) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=1, deprecated=2, removed=3, note="use xyz instead", legacy=false)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "1");
  ASSERT_COMPILED(library);
}

TEST(VersioningTests, GoodAllArgumentsOnMember) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    @available(added=1, deprecated=2, removed=3, note="use xyz instead", legacy=false)
    member string;
};
)FIDL");
  library.SelectVersion("example", "1");
  ASSERT_COMPILED(library);
}

TEST(VersioningTests, GoodAttributeOnEverything) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=1)
const CONST uint32 = 1;

@available(added=1)
alias Alias = string;

// TODO(https://fxbug.dev/42158155): Uncomment.
// @available(added=1)
// type Type = string;

@available(added=1)
type Bits = bits {
    @available(added=1)
    MEMBER = 1;
};

@available(added=1)
type Enum = enum {
    @available(added=1)
    MEMBER = 1;
};

@available(added=1)
type Struct = struct {
    @available(added=1)
    member string;
};

@available(added=1)
type Table = table {
    @available(added=1)
    1: member string;
};

@available(added=1)
type Union = union {
    @available(added=1)
    1: member string;
};

@available(added=1)
protocol ProtocolToCompose {};

@available(added=1)
protocol Protocol {
    @available(added=1)
    compose ProtocolToCompose;

    @available(added=1)
    Method() -> ();
};

@available(added=1)
service Service {
    @available(added=1)
    member client_end:Protocol;
};

@available(added=1)
resource_definition Resource : uint32 {
    properties {
        @available(added=1)
        subtype flexible enum : uint32 {};
    };
};
)FIDL");
  library.SelectVersion("example", "1");
  ASSERT_COMPILED(library);

  auto& unfiltered_decls = library.all_libraries()->Lookup("example")->declaration_order;
  auto& filtered_decls = library.declaration_order();
  // Because everything has the same availability, nothing gets split.
  EXPECT_EQ(unfiltered_decls.size(), filtered_decls.size());
}

// TODO(https://fxbug.dev/42146818): Currently attributes `@HERE type Foo = struct {};` and
// `type Foo = @HERE struct {};` are interchangeable. We just disallow using
// both at once (ErrRedundantAttributePlacement). However, @available on the
// anonymous layout is confusing so maybe we should rethink this design.
TEST(VersioningTests, GoodAttributeOnAnonymousLayoutTopLevel) {
  auto source = R"FIDL(
@available(added=1)
library example;

type Foo = @available(added=2) struct {};
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
    ASSERT_NE(library.LookupStruct("Foo"), nullptr);
  }
}

TEST(VersioningTests, BadAttributeOnAnonymousLayoutInMember) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    member @available(added=2) struct {};
};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrInvalidAttributePlacement, "available");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadInvalidVersionBelowMin) {
  TestLibrary library(R"FIDL(
@available(added=0)
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrInvalidVersion, 0);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadInvalidVersionAboveMaxNumeric) {
  TestLibrary library(R"FIDL(
@available(added=9223372036854775808) // 2^63
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrInvalidVersion, 9223372036854775808u);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadInvalidVersionBeforeHeadOrdinal) {
  TestLibrary library(R"FIDL(
@available(added=18446744073709551613) // 2^64-3
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrInvalidVersion, 18446744073709551613u);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, GoodVersionHeadOrdinal) {
  TestLibrary library(R"FIDL(
@available(added=18446744073709551614) // 2^64-2
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  ASSERT_COMPILED(library);
}

TEST(VersioningTests, BadInvalidVersionLegacyOrdinal) {
  TestLibrary library(R"FIDL(
@available(added=18446744073709551615) // 2^64-1
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrInvalidVersion, 18446744073709551615u);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadInvalidVersionAfterLegacyOrdinal) {
  TestLibrary library(R"FIDL(
@available(added=18446744073709551616) // 2^64
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrCouldNotResolveAttributeArg);
  library.ExpectFail(ErrConstantOverflowsType, "18446744073709551616", "uint64");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadInvalidVersionLegacy) {
  TestLibrary library(R"FIDL(
@available(added=LEGACY)
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrAttributeArgRequiresLiteral, "added", "available");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadInvalidVersionNegative) {
  TestLibrary library(R"FIDL(
@available(added=-1)
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrCouldNotResolveAttributeArg);
  library.ExpectFail(ErrConstantOverflowsType, "-1", "uint64");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadNoArguments) {
  TestLibrary library;
  library.AddFile("bad/fi-0147.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(ErrAvailableMissingArguments);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadLibraryMissingAddedOnlyRemoved) {
  TestLibrary library;
  library.AddFile("bad/fi-0150-a.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(ErrLibraryAvailabilityMissingAdded);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadLibraryMissingAddedOnlyPlatform) {
  TestLibrary library;
  library.AddFile("bad/fi-0150-b.test.fidl");
  library.SelectVersion("foo", "HEAD");
  library.ExpectFail(ErrLibraryAvailabilityMissingAdded);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadLibraryReplaced) {
  TestLibrary library;
  library.AddFile("bad/fi-0204.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(ErrLibraryReplaced);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadNoteWithoutDeprecation) {
  TestLibrary library;
  library.AddFile("bad/fi-0148.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(ErrNoteWithoutDeprecation);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadRemovedAndReplaced) {
  TestLibrary library;
  library.AddFile("bad/fi-0203.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(ErrRemovedAndReplaced);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadPlatformNotOnLibrary) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(platform="bad")
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrPlatformNotOnLibrary);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadUseInUnversionedLibrary) {
  TestLibrary library;
  library.AddFile("bad/fi-0151.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(ErrMissingLibraryAvailability, "test.bad.fi0151");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadUseInUnversionedLibraryReportedOncePerAttribute) {
  TestLibrary library(R"FIDL(
library example;

@available(added=1)
type Foo = struct {
    @available(added=2)
    member1 bool;
    member2 bool;
};
)FIDL");
  library.SelectVersion("example", "HEAD");
  // Note: Only twice, not a third time for member2.
  library.ExpectFail(ErrMissingLibraryAvailability, "example");
  library.ExpectFail(ErrMissingLibraryAvailability, "example");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadAddedEqualsRemoved) {
  TestLibrary library;
  library.AddFile("bad/fi-0154-a.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(ErrInvalidAvailabilityOrder, "added < removed");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadAddedEqualsReplaced) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=2, replaced=2)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrInvalidAvailabilityOrder, "added < replaced");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadAddedGreaterThanRemoved) {
  TestLibrary library(R"FIDL(
@available(added=2, removed=1)
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrInvalidAvailabilityOrder, "added < removed");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadAddedGreaterThanReplaced) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=3, replaced=2)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrInvalidAvailabilityOrder, "added < replaced");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, GoodAddedEqualsDeprecated) {
  TestLibrary library(R"FIDL(
@available(added=1, deprecated=1)
library example;
)FIDL");
  library.SelectVersion("example", "1");
  ASSERT_COMPILED(library);
}

TEST(VersioningTests, BadAddedGreaterThanDeprecated) {
  TestLibrary library(R"FIDL(
@available(added=2, deprecated=1)
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrInvalidAvailabilityOrder, "added <= deprecated");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadDeprecatedEqualsRemoved) {
  TestLibrary library;
  library.AddFile("bad/fi-0154-b.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(ErrInvalidAvailabilityOrder, "added <= deprecated < removed");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadDeprecatedEqualsReplaced) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=1, deprecated=2, replaced=2)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrInvalidAvailabilityOrder, "added <= deprecated < replaced");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadDeprecatedGreaterThanRemoved) {
  TestLibrary library(R"FIDL(
@available(added=1, deprecated=3, removed=2)
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrInvalidAvailabilityOrder, "added <= deprecated < removed");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadDeprecatedGreaterThanReplaced) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=1, deprecated=3, replaced=2)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrInvalidAvailabilityOrder, "added <= deprecated < replaced");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadLegacyTrueNotRemoved) {
  TestLibrary library(R"FIDL(
@available(added=1, legacy=true)
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrLegacyWithoutRemoval, "legacy");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadLegacyFalseNotRemoved) {
  TestLibrary library(R"FIDL(
@available(added=1, legacy=false)
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrLegacyWithoutRemoval, "legacy");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadLegacyTrueNotRemovedMethod) {
  TestLibrary library;
  library.AddFile("bad/fi-0182.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(ErrLegacyWithoutRemoval, "legacy");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, GoodRedundantWithParent) {
  TestLibrary library(R"FIDL(
@available(added=2, deprecated=4, removed=6)
library example;

@available(added=2, deprecated=4, removed=6)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "2");
  ASSERT_COMPILED(library);
}

TEST(VersioningTests, BadAddedBeforeParentAdded) {
  TestLibrary library;
  library.AddFile("bad/fi-0155-a.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(ErrAvailabilityConflictsWithParent, "added", "1", "added", "2",
                     "bad/fi-0155-a.test.fidl:4:12", "added", "before", "added");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, GoodAddedWhenParentDeprecated) {
  TestLibrary library(R"FIDL(
@available(added=2, deprecated=4, removed=6)
library example;

@available(added=4)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "4");
  ASSERT_COMPILED(library);

  auto foo = library.LookupStruct("Foo");
  ASSERT_NE(foo, nullptr);
  EXPECT_TRUE(foo->availability.is_deprecated());
}

TEST(VersioningTests, GoodAddedAfterParentDeprecated) {
  TestLibrary library(R"FIDL(
@available(added=2, deprecated=4, removed=6)
library example;

@available(added=5)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "5");
  ASSERT_COMPILED(library);

  auto foo = library.LookupStruct("Foo");
  ASSERT_NE(foo, nullptr);
  EXPECT_TRUE(foo->availability.is_deprecated());
}

TEST(VersioningTests, BadAddedWhenParentRemoved) {
  TestLibrary library;
  library.AddFile("bad/fi-0155-b.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(ErrAvailabilityConflictsWithParent, "added", "4", "removed", "4",
                     "bad/fi-0155-b.test.fidl:4:35", "added", "after", "removed");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadAddedWhenParentReplaced) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=2, deprecated=4, replaced=6)
type Foo = struct {
    @available(added=6)
    member bool;
};

@available(added=6)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrAvailabilityConflictsWithParent, "added", "6", "replaced", "6",
                     "example.fidl:5:35", "added", "after", "replaced");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadAddedAfterParentRemoved) {
  TestLibrary library(R"FIDL(
@available(added=2, deprecated=4, removed=6)
library example;

@available(added=7)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrAvailabilityConflictsWithParent, "added", "7", "removed", "6",
                     "example.fidl:2:35", "added", "after", "removed");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadAddedAfterParentReplaced) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=2, deprecated=4, replaced=6)
type Foo = struct {
    @available(added=7)
    member bool;
};

@available(added=6)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrAvailabilityConflictsWithParent, "added", "7", "replaced", "6",
                     "example.fidl:5:35", "added", "after", "replaced");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadDeprecatedBeforeParentAdded) {
  TestLibrary library(R"FIDL(
@available(added=2, deprecated=4, removed=6)
library example;

@available(deprecated=1)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrAvailabilityConflictsWithParent, "deprecated", "1", "added", "2",
                     "example.fidl:2:12", "deprecated", "before", "added");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, GoodDeprecatedWhenParentAdded) {
  TestLibrary library(R"FIDL(
@available(added=2, removed=6) // never deprecated
library example;

@available(deprecated=2)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "2");
  ASSERT_COMPILED(library);

  auto foo = library.LookupStruct("Foo");
  ASSERT_NE(foo, nullptr);
  EXPECT_TRUE(foo->availability.is_deprecated());
}

TEST(VersioningTests, GoodDeprecatedBeforeParentDeprecated) {
  TestLibrary library(R"FIDL(
@available(added=2, deprecated=4, removed=6)
library example;

@available(deprecated=3)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "3");
  ASSERT_COMPILED(library);

  auto foo = library.LookupStruct("Foo");
  ASSERT_NE(foo, nullptr);
  EXPECT_TRUE(foo->availability.is_deprecated());
}

TEST(VersioningTests, BadDeprecatedAfterParentDeprecated) {
  TestLibrary library(R"FIDL(
@available(added=2, deprecated=4, removed=6)
library example;

@available(deprecated=5)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrAvailabilityConflictsWithParent, "deprecated", "5", "deprecated", "4",
                     "example.fidl:2:21", "deprecated", "after", "deprecated");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadDeprecatedWhenParentRemoved) {
  TestLibrary library(R"FIDL(
@available(added=2, deprecated=4, removed=6)
library example;

@available(deprecated=6)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrAvailabilityConflictsWithParent, "deprecated", "6", "removed", "6",
                     "example.fidl:2:35", "deprecated", "after", "removed");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadDeprecatedWhenParentReplaced) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=2, deprecated=4, replaced=6)
type Foo = struct {
    @available(deprecated=6)
    member bool;
};

@available(added=6)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrAvailabilityConflictsWithParent, "deprecated", "6", "replaced", "6",
                     "example.fidl:5:35", "deprecated", "after", "replaced");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadDeprecatedAfterParentRemoved) {
  TestLibrary library(R"FIDL(
@available(added=2, deprecated=4, removed=6)
library example;

@available(deprecated=7)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrAvailabilityConflictsWithParent, "deprecated", "7", "removed", "6",
                     "example.fidl:2:35", "deprecated", "after", "removed");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadDeprecatedAfterParentReplaced) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=2, deprecated=4, replaced=6)
type Foo = struct {
    @available(deprecated=7)
    member bool;
};

@available(added=6)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrAvailabilityConflictsWithParent, "deprecated", "7", "replaced", "6",
                     "example.fidl:5:35", "deprecated", "after", "replaced");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadRemovedBeforeParentAdded) {
  TestLibrary library(R"FIDL(
@available(added=2, deprecated=4, removed=6)
library example;

@available(removed=1)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrAvailabilityConflictsWithParent, "removed", "1", "added", "2",
                     "example.fidl:2:12", "removed", "before", "added");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadReplacedBeforeParentAdded) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=2, deprecated=4, removed=6)
type Foo = struct {
    @available(replaced=1)
    member bool;
};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrAvailabilityConflictsWithParent, "replaced", "1", "added", "2",
                     "example.fidl:5:12", "replaced", "before", "added");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadRemovedWhenParentAdded) {
  TestLibrary library(R"FIDL(
@available(added=2, deprecated=4, removed=6)
library example;

@available(removed=2)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrAvailabilityConflictsWithParent, "removed", "2", "added", "2",
                     "example.fidl:2:12", "removed", "before", "added");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadReplacedWhenParentAdded) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=2, deprecated=4, removed=6)
type Foo = struct {
    @available(replaced=2)
    member bool;
};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrAvailabilityConflictsWithParent, "replaced", "2", "added", "2",
                     "example.fidl:5:12", "replaced", "before", "added");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, GoodRemovedBeforeParentDeprecated) {
  TestLibrary library(R"FIDL(
@available(added=2, deprecated=4, removed=6)
library example;

@available(removed=3)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "2");
  ASSERT_COMPILED(library);

  auto foo = library.LookupStruct("Foo");
  ASSERT_NE(foo, nullptr);
  EXPECT_FALSE(foo->availability.is_deprecated());
}

TEST(VersioningTests, GoodReplacedBeforeParentDeprecated) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=2, deprecated=4, removed=6)
type Foo = struct {
    @available(replaced=3)
    member bool;
    @available(added=3)
    member uint32;
};
)FIDL");
  library.SelectVersion("example", "2");
  ASSERT_COMPILED(library);

  auto foo = library.LookupStruct("Foo");
  ASSERT_NE(foo, nullptr);
  EXPECT_FALSE(foo->availability.is_deprecated());
}

TEST(VersioningTests, GoodRemovedWhenParentDeprecated) {
  TestLibrary library(R"FIDL(
@available(added=2, deprecated=4, removed=6)
library example;

@available(removed=4)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "3");
  ASSERT_COMPILED(library);

  auto foo = library.LookupStruct("Foo");
  ASSERT_NE(foo, nullptr);
  EXPECT_FALSE(foo->availability.is_deprecated());
}

TEST(VersioningTests, GoodReplacedWhenParentDeprecated) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=2, deprecated=4, removed=6)
type Foo = struct {
    @available(replaced=4)
    member bool;
    @available(added=4)
    member uint32;
};
)FIDL");
  library.SelectVersion("example", "3");
  ASSERT_COMPILED(library);

  auto foo = library.LookupStruct("Foo");
  ASSERT_NE(foo, nullptr);
  EXPECT_FALSE(foo->availability.is_deprecated());
}

TEST(VersioningTests, GoodRemovedAfterParentDeprecated) {
  TestLibrary library(R"FIDL(
@available(added=2, deprecated=4, removed=6)
library example;

@available(removed=5)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "4");
  ASSERT_COMPILED(library);

  auto foo = library.LookupStruct("Foo");
  ASSERT_NE(foo, nullptr);
  EXPECT_TRUE(foo->availability.is_deprecated());
}

TEST(VersioningTests, GoodReplacedAfterParentDeprecated) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=2, deprecated=4, removed=6)
type Foo = struct {
    @available(replaced=5)
    member bool;
    @available(added=5)
    member uint32;
};
)FIDL");
  library.SelectVersion("example", "4");
  ASSERT_COMPILED(library);

  auto foo = library.LookupStruct("Foo");
  ASSERT_NE(foo, nullptr);
  EXPECT_TRUE(foo->availability.is_deprecated());
}

TEST(VersioningTests, BadRemovedAfterParentRemoved) {
  TestLibrary library(R"FIDL(
@available(added=2, deprecated=4, removed=6)
library example;

@available(removed=7)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrAvailabilityConflictsWithParent, "removed", "7", "removed", "6",
                     "example.fidl:2:35", "removed", "after", "removed");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadRemovedAfterParentReplaced) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=2, deprecated=4, replaced=6)
type Foo = struct {
    @available(removed=7)
    member bool;
};

@available(added=6)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrAvailabilityConflictsWithParent, "removed", "7", "replaced", "6",
                     "example.fidl:5:35", "removed", "after", "replaced");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadReplacedAfterParentRemoved) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=2, deprecated=4, removed=6)
type Foo = struct {
    @available(replaced=7)
    member bool;
};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrAvailabilityConflictsWithParent, "replaced", "7", "removed", "6",
                     "example.fidl:5:35", "replaced", "after", "removed");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadReplacedAfterParentReplaced) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=2, deprecated=4, replaced=6)
type Foo = struct {
    @available(replaced=7)
    member bool;
};

@available(added=6)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrAvailabilityConflictsWithParent, "replaced", "7", "replaced", "6",
                     "example.fidl:5:35", "replaced", "after", "replaced");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, GoodRemovedWhenParentReplaced) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=2, deprecated=4, replaced=6)
type Foo = struct {
    @available(added=2, deprecated=4, removed=6)
    member bool;
};

@available(added=6)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "6");
  ASSERT_COMPILED(library);

  auto foo = library.LookupStruct("Foo");
  ASSERT_NE(foo, nullptr);
  EXPECT_TRUE(foo->members.empty());
}

TEST(VersioningTests, BadReplacedWhenParentRemoved) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=2, deprecated=4, removed=6)
type Foo = struct {
    @available(added=2, deprecated=4, replaced=6)
    member bool;
};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrReplacedWithoutReplacement, "member", Version::From(6).value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadReplacedWhenParentReplaced) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=2, deprecated=4, replaced=6)
type Foo = struct {
    @available(added=2, deprecated=4, replaced=6)
    member bool;
};

@available(added=6)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrReplacedWithoutReplacement, "member", Version::From(6).value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, GoodLegacyParentNotRemovedChildFalse) {
  TestLibrary library(R"FIDL(
@available(added=2, deprecated=4)
library example;

@available(removed=6, legacy=false)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "LEGACY");
  ASSERT_COMPILED(library);
  ASSERT_EQ(library.LookupStruct("Foo"), nullptr);
}

TEST(VersioningTests, GoodLegacyParentNotRemovedChildTrue) {
  TestLibrary library(R"FIDL(
@available(added=2, deprecated=4)
library example;

@available(removed=6, legacy=true)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "LEGACY");
  ASSERT_COMPILED(library);
  ASSERT_NE(library.LookupStruct("Foo"), nullptr);
}

TEST(VersioningTests, GoodLegacyParentFalseChildFalse) {
  TestLibrary library(R"FIDL(
@available(added=2, deprecated=4, removed=6, legacy=false)
library example;

@available(legacy=false)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "LEGACY");
  ASSERT_COMPILED(library);
  ASSERT_EQ(library.LookupStruct("Foo"), nullptr);
}

TEST(VersioningTests, BadLegacyParentFalseChildTrue) {
  TestLibrary library(R"FIDL(
@available(added=2, deprecated=4, removed=6, legacy=false)
library example;

@available(legacy=true)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrLegacyConflictsWithParent, "legacy", "true", "removed", "6",
                     "example.fidl:2:35");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadLegacyParentFalseChildTrueMethod) {
  TestLibrary library;
  library.AddFile("bad/fi-0183.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(ErrLegacyConflictsWithParent, "legacy", "true", "removed", "3",
                     "bad/fi-0183.test.fidl:7:12");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, GoodLegacyParentTrueChildTrue) {
  TestLibrary library(R"FIDL(
@available(added=2, deprecated=4, removed=6, legacy=true)
library example;

@available(legacy=true)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "LEGACY");
  ASSERT_COMPILED(library);
  ASSERT_NE(library.LookupStruct("Foo"), nullptr);
}

TEST(VersioningTests, GoodLegacyParentTrueChildFalse) {
  TestLibrary library(R"FIDL(
@available(added=2, deprecated=4, removed=6, legacy=true)
library example;

@available(legacy=false)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "LEGACY");
  ASSERT_COMPILED(library);
  ASSERT_EQ(library.LookupStruct("Foo"), nullptr);
}

TEST(VersioningTests, GoodMemberInheritsFromParent) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=2)
type Foo = struct {
    @available(deprecated=3)
    member1 bool;
};
)FIDL");
  library.SelectVersion("example", "2");
  ASSERT_COMPILED(library);

  auto foo = library.LookupStruct("Foo");
  ASSERT_NE(foo, nullptr);
  EXPECT_EQ(foo->members.size(), 1u);
}

TEST(VersioningTests, GoodComplexInheritance) {
  // The following libraries all define a struct Bar with effective availability
  // @available(added=2, deprecated=3, removed=4, legacy=true) in different ways.

  std::vector<const char*> sources;

  // Direct annotation.
  sources.push_back(R"FIDL(
@available(added=1)
library example;

@available(added=2, deprecated=3, removed=4, legacy=true)
type Bar = struct {};
)FIDL");

  // Fully inherit from library declaration.
  sources.push_back(R"FIDL(
@available(added=2, deprecated=3, removed=4, legacy=true)
library example;

type Bar = struct {};
)FIDL");

  // Partially inherit from library declaration.
  sources.push_back(R"FIDL(
@available(added=1, deprecated=3)
library example;

@available(added=2, removed=4, legacy=true)
type Bar = struct {};
)FIDL");

  // Inherit from parent.
  sources.push_back(R"FIDL(
@available(added=1)
library example;

@available(added=2, deprecated=3, removed=4, legacy=true)
type Foo = struct {
    member @generated_name("Bar") struct {};
};
)FIDL");

  // Inherit from member.
  sources.push_back(R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    @available(added=2, deprecated=3, removed=4, legacy=true)
    member @generated_name("Bar") struct {};
};
)FIDL");

  // Inherit from multiple, forward.
  sources.push_back(R"FIDL(
@available(added=2)
library example;

@available(deprecated=3)
type Foo = struct {
    @available(removed=4, legacy=true)
    member @generated_name("Bar") struct {};
};
)FIDL");

  // Inherit from multiple, backward.
  sources.push_back(R"FIDL(
@available(added=1, removed=4, legacy=true)
library example;

@available(deprecated=3)
type Foo = struct {
    @available(added=2)
    member @generated_name("Bar") struct {};
};
)FIDL");

  // Inherit from multiple, mixed.
  sources.push_back(R"FIDL(
@available(added=1)
library example;

@available(added=2)
type Foo = struct {
    @available(deprecated=3, removed=4, legacy=true)
    member @generated_name("Bar") struct {};
};
)FIDL");

  // Inherit via nested layouts.
  sources.push_back(R"FIDL(
@available(added=1)
library example;

@available(added=2)
type Foo = struct {
    @available(deprecated=3)
    member1 struct {
        @available(removed=4, legacy=true)
        member2 struct {
            member3 @generated_name("Bar") struct {};
        };
    };
};
)FIDL");

  // Inherit via nested type constructors.
  sources.push_back(R"FIDL(
@available(added=1)
library example;

@available(added=2)
type Foo = struct {
    @available(deprecated=3, removed=4, legacy=true)
    member1 vector<vector<vector<@generated_name("Bar") struct{}>>>;
};
)FIDL");

  for (auto& source : sources) {
    {
      TestLibrary library(source);
      library.SelectVersion("example", "1");
      ASSERT_COMPILED(library);

      auto bar = library.LookupStruct("Bar");
      ASSERT_EQ(bar, nullptr);
    }
    {
      TestLibrary library(source);
      library.SelectVersion("example", "2");
      ASSERT_COMPILED(library);

      auto bar = library.LookupStruct("Bar");
      ASSERT_NE(bar, nullptr);
      EXPECT_FALSE(bar->availability.is_deprecated());
    }
    {
      TestLibrary library(source);
      library.SelectVersion("example", "3");
      ASSERT_COMPILED(library);

      auto bar = library.LookupStruct("Bar");
      ASSERT_NE(bar, nullptr);
      EXPECT_TRUE(bar->availability.is_deprecated());
    }
    {
      TestLibrary library(source);
      library.SelectVersion("example", "4");
      ASSERT_COMPILED(library);

      auto bar = library.LookupStruct("Bar");
      ASSERT_EQ(bar, nullptr);
    }
    {
      TestLibrary library(source);
      library.SelectVersion("example", "LEGACY");
      ASSERT_COMPILED(library);

      auto bar = library.LookupStruct("Bar");
      ASSERT_NE(bar, nullptr);
    }
  }
}

TEST(VersioningTests, BadDeclConflictsWithParent) {
  TestLibrary library(R"FIDL( // L1
@available(added=2)           // L2
library example;              // L3
                              // L4
@available(added=1)           // L5
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrAvailabilityConflictsWithParent, "added", "1", "added", "2",
                     "example.fidl:2:12", "added", "before", "added");
  ASSERT_COMPILER_DIAGNOSTICS(library);
  EXPECT_EQ(library.errors()[0]->span.position().line, 5);
}

TEST(VersioningTests, BadMemberConflictsWithParent) {
  TestLibrary library(R"FIDL( // L1
@available(added=1)           // L2
library example;              // L3
                              // L4
@available(added=2)           // L5
type Foo = struct {           // L6
    @available(added=1)       // L7
    member1 bool;
};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrAvailabilityConflictsWithParent, "added", "1", "added", "2",
                     "example.fidl:5:12", "added", "before", "added");
  ASSERT_COMPILER_DIAGNOSTICS(library);
  EXPECT_EQ(library.errors()[0]->span.position().line, 7);
}

TEST(VersioningTests, BadMemberConflictsWithGrandParent) {
  TestLibrary library(R"FIDL( // L1
@available(added=2)           // L2
library example;              // L3
                              // L4
@available(removed=3)         // L5
type Foo = struct {           // L6
    @available(added=1)       // L7
    member1 bool;
};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrAvailabilityConflictsWithParent, "added", "1", "added", "2",
                     "example.fidl:2:12", "added", "before", "added");
  ASSERT_COMPILER_DIAGNOSTICS(library);
  EXPECT_EQ(library.errors()[0]->span.position().line, 7);
}

TEST(VersioningTests, BadMemberConflictsWithGrandParentThroughAnonymous) {
  TestLibrary library(R"FIDL( // L1
@available(added=1)           // L2
library example;              // L3
                              // L4
@available(added=2)           // L5
type Foo = struct {           // L6
    member1 struct {          // L7
        @available(removed=1) // L8
        member2 bool;
    };
};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrAvailabilityConflictsWithParent, "removed", "1", "added", "2",
                     "example.fidl:5:12", "removed", "before", "added");
  ASSERT_COMPILER_DIAGNOSTICS(library);
  EXPECT_EQ(library.errors()[0]->span.position().line, 8);
}

TEST(VersioningTests, BadLegacyConflictsWithRemoved) {
  TestLibrary library(R"FIDL(  // L1
@available(added=1, removed=2) // L2
library example;               // L3
                               // L4
@available(legacy=true)        // L5
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrLegacyConflictsWithParent, "legacy", "true", "removed", "2",
                     "example.fidl:2:21");
  ASSERT_COMPILER_DIAGNOSTICS(library);
  EXPECT_EQ(library.errors()[0]->span.position().line, 5);
}

TEST(VersioningTests, BadRemovedWithReplacement) {
  TestLibrary library;
  library.AddFile("bad/fi-0205.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(ErrRemovedWithReplacement, "Bar", Version::From(2).value(),
                     "bad/fi-0205.test.fidl:11:14");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadRemovedNamedToAnonymous) {
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
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrRemovedWithReplacement, "Foo", Version::From(2).value(),
                     "example.fidl:10:9");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, GoodRemovedAnonymousToNamed) {
  auto source = R"FIDL(
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
)FIDL";

  {
    TestLibrary library(source);
    library.SelectVersion("example", "1");
    ASSERT_COMPILED(library);

    EXPECT_NE(library.LookupStruct("Foo"), nullptr);
    EXPECT_EQ(library.LookupTable("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "2");
    ASSERT_COMPILED(library);

    EXPECT_EQ(library.LookupStruct("Foo"), nullptr);
    EXPECT_NE(library.LookupTable("Foo"), nullptr);
  }
}

TEST(VersioningTests, GoodRemovedAnonymousToAnonymous) {
  auto source = R"FIDL(
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
)FIDL";

  {
    TestLibrary library(source);
    library.SelectVersion("example", "1");
    ASSERT_COMPILED(library);

    EXPECT_NE(library.LookupStruct("Foo"), nullptr);
    EXPECT_EQ(library.LookupTable("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "2");
    ASSERT_COMPILED(library);

    EXPECT_EQ(library.LookupStruct("Foo"), nullptr);
    EXPECT_NE(library.LookupTable("Foo"), nullptr);
  }
}

TEST(VersioningTests, BadReplacedWithoutReplacement) {
  TestLibrary library;
  library.AddFile("bad/fi-0206.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(ErrReplacedWithoutReplacement, "Bar", Version::From(2).value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, GoodAnonymousReplacedWithoutReplacement) {
  auto source = R"FIDL(
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
)FIDL";

  {
    TestLibrary library(source);
    library.SelectVersion("example", "1");
    ASSERT_COMPILED(library);

    EXPECT_NE(library.LookupStruct("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "2");
    ASSERT_COMPILED(library);

    EXPECT_EQ(library.LookupStruct("Foo"), nullptr);
  }
}

TEST(VersioningTests, GoodReplacedNamedToAnonymous) {
  auto source = R"FIDL(
@available(added=1)
library example;

@available(replaced=2)
type Foo = struct {};

type Bar = struct {
    @available(added=2)
    foo table {};
};
)FIDL";

  {
    TestLibrary library(source);
    library.SelectVersion("example", "1");
    ASSERT_COMPILED(library);

    EXPECT_NE(library.LookupStruct("Foo"), nullptr);
    EXPECT_EQ(library.LookupTable("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "2");
    ASSERT_COMPILED(library);

    EXPECT_EQ(library.LookupStruct("Foo"), nullptr);
    EXPECT_NE(library.LookupTable("Foo"), nullptr);
  }
}

TEST(VersioningTests, GoodReplacedAnonymousToNamed) {
  auto source = R"FIDL(
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
)FIDL";

  {
    TestLibrary library(source);
    library.SelectVersion("example", "1");
    ASSERT_COMPILED(library);

    EXPECT_NE(library.LookupStruct("Foo"), nullptr);
    EXPECT_EQ(library.LookupTable("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "2");
    ASSERT_COMPILED(library);

    EXPECT_EQ(library.LookupStruct("Foo"), nullptr);
    EXPECT_NE(library.LookupTable("Foo"), nullptr);
  }
}

TEST(VersioningTests, GoodReplacedAnonymousToAnonymous) {
  auto source = R"FIDL(
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
)FIDL";

  {
    TestLibrary library(source);
    library.SelectVersion("example", "1");
    ASSERT_COMPILED(library);

    EXPECT_NE(library.LookupStruct("Foo"), nullptr);
    EXPECT_EQ(library.LookupTable("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "2");
    ASSERT_COMPILED(library);

    EXPECT_EQ(library.LookupStruct("Foo"), nullptr);
    EXPECT_NE(library.LookupTable("Foo"), nullptr);
  }
}

TEST(VersioningTests, GoodReplacedTwice) {
  auto source = R"FIDL(
@available(added=1)
library example;

@available(replaced=2)
type Foo = struct {};

@available(added=2, replaced=3)
type Foo = table {};

@available(added=3)
type Foo = union {};
)FIDL";

  {
    TestLibrary library(source);
    library.SelectVersion("example", "1");
    ASSERT_COMPILED(library);

    EXPECT_NE(library.LookupStruct("Foo"), nullptr);
    EXPECT_EQ(library.LookupTable("Foo"), nullptr);
    EXPECT_EQ(library.LookupUnion("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "2");
    ASSERT_COMPILED(library);

    EXPECT_EQ(library.LookupStruct("Foo"), nullptr);
    EXPECT_NE(library.LookupTable("Foo"), nullptr);
    EXPECT_EQ(library.LookupUnion("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "3");
    ASSERT_COMPILED(library);

    EXPECT_EQ(library.LookupStruct("Foo"), nullptr);
    EXPECT_EQ(library.LookupTable("Foo"), nullptr);
    EXPECT_NE(library.LookupUnion("Foo"), nullptr);
  }
}

TEST(VersioningTests, GoodNonOverlappingNamesNoGap) {
  auto source = R"FIDL(
@available(added=1)
library example;

@available(replaced=2)
type Foo = struct {};

@available(added=2)
type Foo = table {};
)FIDL";

  {
    TestLibrary library(source);
    library.SelectVersion("example", "1");
    ASSERT_COMPILED(library);

    EXPECT_NE(library.LookupStruct("Foo"), nullptr);
    EXPECT_EQ(library.LookupTable("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "2");
    ASSERT_COMPILED(library);

    EXPECT_EQ(library.LookupStruct("Foo"), nullptr);
    EXPECT_NE(library.LookupTable("Foo"), nullptr);
  }
}

TEST(VersioningTests, GoodNonOverlappingNamesWithGap) {
  auto source = R"FIDL(
@available(added=1)
library example;

@available(removed=2)
type Foo = struct {};

@available(added=3)
type Foo = table {};
)FIDL";

  {
    TestLibrary library(source);
    library.SelectVersion("example", "1");
    ASSERT_COMPILED(library);

    EXPECT_NE(library.LookupStruct("Foo"), nullptr);
    EXPECT_EQ(library.LookupTable("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "2");
    ASSERT_COMPILED(library);

    EXPECT_EQ(library.LookupStruct("Foo"), nullptr);
    EXPECT_EQ(library.LookupTable("Foo"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "3");
    ASSERT_COMPILED(library);

    EXPECT_EQ(library.LookupStruct("Foo"), nullptr);
    EXPECT_NE(library.LookupTable("Foo"), nullptr);
  }
}

TEST(VersioningTests, GoodNonOverlappingNamesNoGapCanonical) {
  auto source = R"FIDL(
@available(added=1)
library example;

@available(removed=2)
type foo = struct {};

@available(added=2)
type FOO = table {};
)FIDL";

  {
    TestLibrary library(source);
    library.SelectVersion("example", "1");
    ASSERT_COMPILED(library);

    EXPECT_NE(library.LookupStruct("foo"), nullptr);
    EXPECT_EQ(library.LookupTable("FOO"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "2");
    ASSERT_COMPILED(library);

    EXPECT_EQ(library.LookupStruct("foo"), nullptr);
    EXPECT_NE(library.LookupTable("FOO"), nullptr);
  }
}

TEST(VersioningTests, GoodNonOverlappingNamesWithGapCanonical) {
  auto source = R"FIDL(
@available(added=1)
library example;

@available(removed=2)
type foo = struct {};

@available(added=3)
type FOO = table {};
)FIDL";

  {
    TestLibrary library(source);
    library.SelectVersion("example", "1");
    ASSERT_COMPILED(library);

    EXPECT_NE(library.LookupStruct("foo"), nullptr);
    EXPECT_EQ(library.LookupTable("FOO"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "2");
    ASSERT_COMPILED(library);

    EXPECT_EQ(library.LookupStruct("foo"), nullptr);
    EXPECT_EQ(library.LookupTable("FOO"), nullptr);
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "3");
    ASSERT_COMPILED(library);

    EXPECT_EQ(library.LookupStruct("foo"), nullptr);
    EXPECT_NE(library.LookupTable("FOO"), nullptr);
  }
}

TEST(VersioningTests, BadOverlappingNamesEqualToOther) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {};
type Foo = table {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrNameCollision, Element::Kind::kTable, "Foo", Element::Kind::kStruct,
                     "example.fidl:5:6");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadOverlappingNamesEqualToOtherLegacy) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(removed=2, legacy=true)
type Foo = struct {};
@available(removed=2, legacy=true)
type Foo = table {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrNameCollision, Element::Kind::kTable, "Foo", Element::Kind::kStruct,
                     "example.fidl:6:6");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadOverlappingNamesEqualToOtherCanonical) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type foo = struct {};
type FOO = table {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrNameCollisionCanonical, Element::Kind::kStruct, "foo",
                     Element::Kind::kTable, "FOO", "example.fidl:6:6", "foo");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadOverlappingNamesSimple) {
  TestLibrary library;
  library.AddFile("bad/fi-0036.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(ErrNameOverlap, Element::Kind::kEnum, "Color", Element::Kind::kEnum,
                     "bad/fi-0036.test.fidl:7:6",
                     VersionSet(VersionRange(Version::From(2).value(), Version::PosInf())),
                     Platform::Parse("test").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, GoodOverlappingNamesSimpleFixAvailability) {
  TestLibrary library;
  library.AddFile("good/fi-0036.test.fidl");
  library.SelectVersion("test", "HEAD");
  ASSERT_COMPILED(library);
}

TEST(VersioningTests, BadOverlappingNamesSimpleCanonical) {
  TestLibrary library;
  library.AddFile("bad/fi-0037.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(ErrNameOverlapCanonical, Element::Kind::kProtocol, "Color",
                     Element::Kind::kConst, "COLOR", "bad/fi-0037.test.fidl:7:7", "color",
                     VersionSet(VersionRange(Version::From(2).value(), Version::PosInf())),
                     Platform::Parse("test").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, GoodOverlappingNamesSimpleCanonicalFixRename) {
  TestLibrary library;
  library.AddFile("good/fi-0037.test.fidl");
  library.SelectVersion("test", "HEAD");
  ASSERT_COMPILED(library);
}

TEST(VersioningTests, BadOverlappingNamesContainsOther) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {};
@available(removed=2)
type Foo = table {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrNameOverlap, Element::Kind::kTable, "Foo", Element::Kind::kStruct,
                     "example.fidl:5:6",
                     VersionSet(VersionRange(Version::From(1).value(), Version::From(2).value())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadOverlappingNamesContainsOtherCanonical) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type foo = struct {};
@available(removed=2)
type FOO = table {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrNameOverlapCanonical, Element::Kind::kStruct, "foo", Element::Kind::kTable,
                     "FOO", "example.fidl:7:6", "foo",
                     VersionSet(VersionRange(Version::From(1).value(), Version::From(2).value())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadOverlappingNamesIntersectsOther) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(removed=5)
type Foo = struct {};
@available(added=3)
type Foo = table {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrNameOverlap, Element::Kind::kTable, "Foo", Element::Kind::kStruct,
                     "example.fidl:6:6",
                     VersionSet(VersionRange(Version::From(3).value(), Version::From(5).value())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadOverlappingNamesIntersectsOtherCanonical) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(removed=5)
type foo = struct {};
@available(added=3)
type FOO = table {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrNameOverlapCanonical, Element::Kind::kStruct, "foo", Element::Kind::kTable,
                     "FOO", "example.fidl:8:6", "foo",
                     VersionSet(VersionRange(Version::From(3).value(), Version::From(5).value())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadOverlappingNamesJustAtLegacy) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(replaced=2, legacy=true)
type Foo = struct {};
@available(added=2, removed=3, legacy=true)
type Foo = table {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrNameOverlap, Element::Kind::kTable, "Foo", Element::Kind::kStruct,
                     "example.fidl:6:6",
                     VersionSet(VersionRange(Version::Legacy(), Version::PosInf())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadOverlappingNamesJustAtLegacyCanonical) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(removed=2, legacy=true)
type foo = struct {};
@available(added=2, removed=3, legacy=true)
type FOO = table {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrNameOverlapCanonical, Element::Kind::kStruct, "foo", Element::Kind::kTable,
                     "FOO", "example.fidl:8:6", "foo",
                     VersionSet(VersionRange(Version::Legacy(), Version::PosInf())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadOverlappingNamesIntersectAtLegacy) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(replaced=2, legacy=true)
type Foo = struct {};
@available(added=2)
type Foo = table {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrNameOverlap, Element::Kind::kTable, "Foo", Element::Kind::kStruct,
                     "example.fidl:6:6",
                     VersionSet(VersionRange(Version::Legacy(), Version::PosInf())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadOverlappingNamesIntersectAtLegacyCanonical) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(removed=2, legacy=true)
type foo = struct {};
@available(added=2)
type FOO = table {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrNameOverlapCanonical, Element::Kind::kStruct, "foo", Element::Kind::kTable,
                     "FOO", "example.fidl:8:6", "foo",
                     VersionSet(VersionRange(Version::Legacy(), Version::PosInf())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadOverlappingNamesMultiple) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {};
@available(added=3)
type Foo = table {};
@available(added=HEAD)
const Foo uint32 = 0;
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrNameOverlap, Element::Kind::kStruct, "Foo", Element::Kind::kConst,
                     "example.fidl:9:7",
                     VersionSet(VersionRange(Version::Head(), Version::PosInf())),
                     Platform::Parse("example").value());
  library.ExpectFail(ErrNameOverlap, Element::Kind::kTable, "Foo", Element::Kind::kStruct,
                     "example.fidl:5:6",
                     VersionSet(VersionRange(Version::From(3).value(), Version::PosInf())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadOverlappingNamesRecursive) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=1, removed=5)
type Foo = struct { member box<Foo>; };

@available(added=3, removed=7)
type Foo = struct { member box<Foo>; };
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrNameOverlap, Element::Kind::kStruct, "Foo", Element::Kind::kStruct,
                     "example.fidl:6:6",
                     VersionSet(VersionRange(Version::From(3).value(), Version::From(5).value())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadOverlappingMemberNamesEqualToOther) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    member bool;
    member bool;
};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrNameCollision, Element::Kind::kStructMember, "member",
                     Element::Kind::kStructMember, "example.fidl:6:5");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadOverlappingMemberNamesEqualToOtherLegacy) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    @available(removed=2, legacy=true)
    member bool;
    @available(removed=2, legacy=true)
    member bool;
};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrNameCollision, Element::Kind::kStructMember, "member",
                     Element::Kind::kStructMember, "example.fidl:7:5");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadOverlappingMemberNamesEqualToOtherCanonical) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    member bool;
    MEMBER bool;
};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrNameCollisionCanonical, Element::Kind::kStructMember, "MEMBER",
                     Element::Kind::kStructMember, "member", "example.fidl:6:5", "member");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadOverlappingMemberNamesContainsOther) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    member bool;
    @available(removed=2)
    member bool;
};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrNameOverlap, Element::Kind::kStructMember, "member",
                     Element::Kind::kStructMember, "example.fidl:6:5",
                     VersionSet(VersionRange(Version::From(1).value(), Version::From(2).value())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadOverlappingMemberNamesContainsOtherCanonical) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    member bool;
    @available(removed=2)
    MEMBER bool;
};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrNameOverlapCanonical, Element::Kind::kStructMember, "MEMBER",
                     Element::Kind::kStructMember, "member", "example.fidl:6:5", "member",
                     VersionSet(VersionRange(Version::From(1).value(), Version::From(2).value())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadOverlappingMemberNamesIntersectsOther) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    @available(removed=5)
    member bool;
    @available(added=3)
    member bool;
};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrNameOverlap, Element::Kind::kStructMember, "member",
                     Element::Kind::kStructMember, "example.fidl:7:5",
                     VersionSet(VersionRange(Version::From(3).value(), Version::From(5).value())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadOverlappingMemberNamesIntersectsOtherCanonical) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    @available(removed=5)
    member bool;
    @available(added=3)
    MEMBER bool;
};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrNameOverlapCanonical, Element::Kind::kStructMember, "MEMBER",
                     Element::Kind::kStructMember, "member", "example.fidl:7:5", "member",
                     VersionSet(VersionRange(Version::From(3).value(), Version::From(5).value())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadOverlappingMemberNamesJustAtLegacy) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    @available(replaced=2, legacy=true)
    member bool;
    @available(added=2, removed=3, legacy=true)
    member bool;
};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrNameOverlap, Element::Kind::kStructMember, "member",
                     Element::Kind::kStructMember, "example.fidl:7:5",
                     VersionSet(VersionRange(Version::Legacy(), Version::PosInf())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadOverlappingMemberNamesJustAtLegacyCanonical) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    @available(removed=2, legacy=true)
    member bool;
    @available(added=2, removed=3, legacy=true)
    MEMBER bool;
};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrNameOverlapCanonical, Element::Kind::kStructMember, "MEMBER",
                     Element::Kind::kStructMember, "member", "example.fidl:7:5", "member",
                     VersionSet(VersionRange(Version::Legacy(), Version::PosInf())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadOverlappingMemberNamesIntersectAtLegacy) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    @available(replaced=2, legacy=true)
    member bool;
    @available(added=2)
    member bool;
};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrNameOverlap, Element::Kind::kStructMember, "member",
                     Element::Kind::kStructMember, "example.fidl:7:5",
                     VersionSet(VersionRange(Version::Legacy(), Version::PosInf())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadOverlappingMemberNamesIntersectAtLegacyCanonical) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    @available(removed=2, legacy=true)
    member bool;
    @available(added=2)
    MEMBER bool;
};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrNameOverlapCanonical, Element::Kind::kStructMember, "MEMBER",
                     Element::Kind::kStructMember, "member", "example.fidl:7:5", "member",
                     VersionSet(VersionRange(Version::Legacy(), Version::PosInf())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadOverlappingMemberNamesMultiple) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    member bool;
    @available(added=3)
    member bool;
    @available(added=HEAD)
    member bool;
};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrNameOverlap, Element::Kind::kStructMember, "member",
                     Element::Kind::kStructMember, "example.fidl:6:5",
                     VersionSet(VersionRange(Version::From(3).value(), Version::PosInf())),
                     Platform::Parse("example").value());
  library.ExpectFail(ErrNameOverlap, Element::Kind::kStructMember, "member",
                     Element::Kind::kStructMember, "example.fidl:6:5",
                     VersionSet(VersionRange(Version::Head(), Version::PosInf())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningTests, BadOverlappingMemberNamesMultipleCanonical) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    member bool;
    @available(added=3)
    Member bool;
    @available(added=HEAD)
    MEMBER bool;
};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrNameOverlapCanonical, Element::Kind::kStructMember, "Member",
                     Element::Kind::kStructMember, "member", "example.fidl:6:5", "member",
                     VersionSet(VersionRange(Version::From(3).value(), Version::PosInf())),
                     Platform::Parse("example").value());
  library.ExpectFail(ErrNameOverlapCanonical, Element::Kind::kStructMember, "MEMBER",
                     Element::Kind::kStructMember, "member", "example.fidl:6:5", "member",
                     VersionSet(VersionRange(Version::Head(), Version::PosInf())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

// TODO(https://fxbug.dev/42052719): Generalize this with more comprehensive tests in
// availability_interleaving_tests.cc.
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

TEST(VersioningTests, GoodMultiplePlatformsBasic) {
  SharedAmongstLibraries shared;
  shared.SelectVersion("dependency", "3");
  shared.SelectVersion("example", "HEAD");

  TestLibrary dependency(&shared, "dependency.fidl", R"FIDL(
@available(added=2)
library dependency;

@available(added=3, deprecated=4, removed=5)
type Foo = struct {};
)FIDL");
  ASSERT_COMPILED(dependency);

  TestLibrary example(&shared, "example.fidl", R"FIDL(
@available(added=1)
library example;

using dependency;

type Foo = struct {
    @available(deprecated=5)
    dep dependency.Foo;
};
)FIDL");
  ASSERT_COMPILED(example);
}

TEST(VersioningTests, GoodMultiplePlatformsExplicitPlatform) {
  SharedAmongstLibraries shared;
  shared.SelectVersion("xyz", "3");
  shared.SelectVersion("example", "HEAD");

  TestLibrary dependency(&shared, "dependency.fidl", R"FIDL(
@available(platform="xyz", added=1)
library dependency;

@available(added=3, removed=4)
type Foo = struct {};
)FIDL");
  ASSERT_COMPILED(dependency);

  TestLibrary example(&shared, "example.fidl", R"FIDL(
@available(added=1)
library example;

using dependency;

alias Foo = dependency.Foo;
)FIDL");
  ASSERT_COMPILED(example);
}

TEST(VersioningTests, GoodMultiplePlatformsUsesCorrectDecl) {
  SharedAmongstLibraries shared;
  shared.SelectVersion("dependency", "4");
  shared.SelectVersion("example", "1");

  TestLibrary dependency(&shared, "dependency.fidl", R"FIDL(
@available(added=2)
library dependency;

@available(deprecated=3, replaced=4)
type Foo = resource struct {};

@available(added=4, removed=5)
type Foo = table {};
)FIDL");
  ASSERT_COMPILED(dependency);

  TestLibrary example(&shared, "example.fidl", R"FIDL(
@available(added=1)
library example;

using dependency;

type Foo = struct {
    dep dependency.Foo;
};
)FIDL");
  ASSERT_COMPILED(example);

  auto foo = example.LookupStruct("Foo");
  ASSERT_NE(foo, nullptr);
  ASSERT_EQ(foo->members.size(), 1u);
  auto member_type = foo->members[0].type_ctor->type;
  ASSERT_EQ(member_type->kind, Type::Kind::kIdentifier);
  auto identifier_type = static_cast<const IdentifierType*>(member_type);
  EXPECT_EQ(identifier_type->type_decl->kind, Decl::Kind::kTable);
}

TEST(VersioningTests, BadMultiplePlatformsNameNotFound) {
  SharedAmongstLibraries shared;
  shared.SelectVersion("dependency", "HEAD");
  shared.SelectVersion("example", "HEAD");

  TestLibrary dependency(&shared, "dependency.fidl", R"FIDL(
@available(added=2)
library dependency;

@available(added=3, removed=5)
type Foo = struct {};
)FIDL");
  ASSERT_COMPILED(dependency);

  TestLibrary example(&shared, "example.fidl", R"FIDL(
@available(added=1)
library example;

using dependency;

type Foo = struct {
    @available(deprecated=5)
    dep dependency.Foo;
};
)FIDL");
  example.ExpectFail(ErrNameNotFound, "Foo", "library 'dependency'");
  example.ExpectFail(ErrNameNotFound, "Foo", "library 'dependency'");
  ASSERT_COMPILER_DIAGNOSTICS(example);
}

TEST(VersioningTests, GoodMixVersionedAndUnversioned) {
  SharedAmongstLibraries shared;
  shared.SelectVersion("example", "1");

  TestLibrary versioned(&shared, "versioned.fidl", R"FIDL(
@available(added=1, removed=2)
library example.versioned;

type Foo = struct {};
)FIDL");
  ASSERT_COMPILED(versioned);

  TestLibrary not_versioned(&shared, "not_versioned.fidl", R"FIDL(
library example.notversioned;

using example.versioned;

alias Foo = example.versioned.Foo;
)FIDL");
  ASSERT_COMPILED(not_versioned);

  // The example.notversioned library is considered added=HEAD in the special
  // "unversioned" platform. (If it were instead in the "example" platform, it
  // would appear empty because we're using `--available example:1`.)
  ASSERT_NE(not_versioned.LookupAlias("Foo"), nullptr);
}

}  // namespace
}  // namespace fidlc
