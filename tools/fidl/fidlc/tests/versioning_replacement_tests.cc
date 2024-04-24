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

TEST_P(VersioningReplacementTest, GoodDeclRemoved) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(removed=2)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.HasStruct("Foo"), GetParam() == V1);
}

TEST_P(VersioningReplacementTest, BadDeclRemoved) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(removed=2)
type Foo = struct {};

@available(added=2)
type Foo = resource struct {};
)FIDL");
  library.SelectVersion("example", GetParam());
  library.ExpectFail(ErrRemovedWithReplacement, "Foo", V2, "example.fidl:9:6");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningReplacementTest, GoodDeclReplaced) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(replaced=2)
type Foo = struct {};

@available(added=2)
type Foo = resource struct {};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.LookupStruct("Foo")->resourceness,
            GetParam() == V1 ? Resourceness::kValue : Resourceness::kResource);
}

TEST_P(VersioningReplacementTest, BadDeclReplaced) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(replaced=2)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", GetParam());
  library.ExpectFail(ErrReplacedWithoutReplacement, "Foo", V2);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningReplacementTest, GoodMemberRemoved) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = table {
    @available(removed=2)
    1: bar string;
};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.LookupTable("Foo")->members.size(), GetParam() == V1 ? 1u : 0u);
}

TEST_P(VersioningReplacementTest, BadMemberRemoved) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = table {
    @available(removed=2)
    1: bar string;
    @available(added=2)
    1: bar uint32;
};
)FIDL");
  library.SelectVersion("example", GetParam());
  library.ExpectFail(ErrRemovedWithReplacement, "bar", V2, "example.fidl:9:8");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningReplacementTest, GoodMemberReplaced) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = table {
    @available(replaced=2)
    1: bar string;
    @available(added=2)
    1: bar uint32;
};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.LookupTable("Foo")->members.front().type_ctor->type->kind,
            GetParam() == V1 ? Type::Kind::kString : Type::Kind::kPrimitive);
}

TEST_P(VersioningReplacementTest, BadMemberReplaced) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = table {
    @available(replaced=2)
    1: bar string;
};
)FIDL");
  library.SelectVersion("example", GetParam());
  library.ExpectFail(ErrReplacedWithoutReplacement, "bar", V2);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningReplacementTest, GoodMethodRemoved) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

protocol Foo {
    @available(removed=2)
    Bar();
};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.LookupProtocol("Foo")->methods.size(), GetParam() == V1 ? 1u : 0u);
}

TEST_P(VersioningReplacementTest, BadMethodRemoved) {
  TestLibrary library;
  library.SelectVersion("test", GetParam());
  library.AddFile("bad/fi-0205.test.fidl");
  library.ExpectFail(ErrRemovedWithReplacement, "Bar", V2, "bad/fi-0205.test.fidl:11:14");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningReplacementTest, GoodMethodReplaced) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

protocol Foo {
    @available(replaced=2)
    strict Bar();
    @available(added=2)
    flexible Bar();
};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.LookupProtocol("Foo")->methods[0].strictness,
            GetParam() == V1 ? Strictness::kStrict : Strictness::kFlexible);
}

TEST_P(VersioningReplacementTest, BadMethodReplaced) {
  TestLibrary library;
  library.AddFile("bad/fi-0206.test.fidl");
  library.SelectVersion("test", GetParam());
  library.ExpectFail(ErrReplacedWithoutReplacement, "Bar", V2);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningReplacementTest, BadMethodRemovedComposeNew) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

protocol Protocol {
  @available(removed=2)
  Method();

  @available(added=2)
  compose Base;
};

protocol Base {
  @selector("example/Protocol.Method")
  Method();
};
)FIDL");
  library.SelectVersion("example", GetParam());
  library.ExpectFail(ErrRemovedWithReplacement, "Method", V2, "example.fidl:15:3");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningReplacementTest, BadMethodRemovedComposeExisting) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

protocol Protocol {
  @available(removed=2)
  Method();

  compose Base;
};

protocol Base {
  @available(added=2)
  @selector("example/Protocol.Method")
  Method();
};
)FIDL");
  library.SelectVersion("example", GetParam());
  library.ExpectFail(ErrRemovedWithReplacement, "Method", V2, "example.fidl:15:3");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningReplacementTest, GoodMethodReplacedComposeNew) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

protocol Protocol {
  @available(replaced=2)
  Method();

  @available(added=2)
  compose Base;
};

protocol Base {
  @selector("example/Protocol.Method")
  Method();
};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.LookupProtocol("Protocol")->all_methods[0].is_composed, GetParam() >= V2);
}

TEST_P(VersioningReplacementTest, GoodMethodReplacedComposeExisting) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

protocol Protocol {
  @available(replaced=2)
  Method();

  compose Base;
};

protocol Base {
  @available(added=2)
  @selector("example/Protocol.Method")
  Method();
};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.LookupProtocol("Protocol")->all_methods[0].is_composed, GetParam() >= V2);
}

TEST_P(VersioningReplacementTest, GoodReplacedTwice) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(replaced=2)
type Foo = struct {};

@available(added=2, replaced=3)
type Foo = struct {};

@available(added=3)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
}

TEST_P(VersioningReplacementTest, GoodAllDeclsReplacedAndRemoved) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(replaced=2)
const CONST uint32 = 1;
@available(added=2, removed=3)
const CONST uint32 = 1;

@available(replaced=2)
alias Alias = string;
@available(added=2, removed=3)
alias Alias = string;

// TODO(https://fxbug.dev/42158155): Uncomment.
// @available(replaced=2)
// type Type = string;
// @available(added=2, removed=2)
// type Type = string;

@available(replaced=2)
type Bits = bits {};
@available(added=2, removed=3)
type Bits = bits {};

@available(replaced=2)
type Enum = enum {};
@available(added=2, removed=3)
type Enum = enum {};

@available(replaced=2)
type Struct = struct {};
@available(added=2, removed=3)
type Struct = struct {};

@available(replaced=2)
type Table = table {};
@available(added=2, removed=3)
type Table = table {};

@available(replaced=2)
type Union = union {};
@available(added=2, removed=3)
type Union = union {};

@available(replaced=2)
protocol Protocol {};
@available(added=2, removed=3)
protocol Protocol {};

@available(replaced=2)
service Service {};
@available(added=2, removed=3)
service Service {};

@available(replaced=2)
resource_definition Resource : uint32 { properties { subtype flexible enum : uint32 {}; }; };
@available(added=2, removed=3)
resource_definition Resource : uint32 { properties { subtype flexible enum : uint32 {}; }; };
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.declaration_order().size(), GetParam() <= V2 ? 11u : 0u);
}

TEST_P(VersioningReplacementTest, GoodAllMembersReplacedAndRemoved) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Bits = bits {
    @available(replaced=2)
    MEMBER = 1;
    @available(added=2, removed=3)
    MEMBER = 1;
};

type Enum = enum {
    @available(replaced=2)
    MEMBER = 1;
    @available(added=2, removed=3)
    MEMBER = 1;
};

type Struct = struct {
    @available(replaced=2)
    member uint32;
    @available(added=2, removed=3)
    member uint32;
};

type Table = table {
    @available(replaced=2)
    1: member uint32;
    @available(added=2, removed=3)
    1: member uint32;
};

type Union = union {
    @available(replaced=2)
    1: member uint32;
    @available(added=2, removed=3)
    1: member uint32;
};

protocol Protocol {
    @available(replaced=2)
    Member();
    @available(added=2, removed=3)
    Member();
};

service Service {
    @available(replaced=2)
    member client_end:Protocol;
    @available(added=2, removed=3)
    member client_end:Protocol;
};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  auto num_members = GetParam() <= V2 ? 1u : 0u;
  EXPECT_EQ(library.LookupBits("Bits")->members.size(), num_members);
  EXPECT_EQ(library.LookupEnum("Enum")->members.size(), num_members);
  EXPECT_EQ(library.LookupStruct("Struct")->members.size(), num_members);
  EXPECT_EQ(library.LookupTable("Table")->members.size(), num_members);
  EXPECT_EQ(library.LookupUnion("Union")->members.size(), num_members);
  EXPECT_EQ(library.LookupProtocol("Protocol")->methods.size(), num_members);
  EXPECT_EQ(library.LookupService("Service")->members.size(), num_members);
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
  library.ExpectFail(ErrRemovedWithReplacement, "Foo", V2, "example.fidl:10:9");
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

TEST_P(VersioningReplacementTest, GoodReplacedAnonymousToNothing) {
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

}  // namespace
}  // namespace fidlc
