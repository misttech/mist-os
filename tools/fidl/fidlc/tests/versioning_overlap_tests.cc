// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "tools/fidl/fidlc/tests/test_library.h"

// This file tests ways of overlapping element availabilities.
// Tests are run for all the versions given in INSTANTIATE_TEST_SUITE_P.

namespace fidlc {
namespace {

class VersioningOverlapTest : public testing::TestWithParam<Version> {};

const Version V1 = Version::From(1).value();
const Version V2 = Version::From(2).value();
const Version V3 = Version::From(3).value();

INSTANTIATE_TEST_SUITE_P(VersioningOverlapTests, VersioningOverlapTest, testing::Values(V1, V2, V3),
                         [](auto info) { return info.param.ToString(); });

TEST_P(VersioningOverlapTest, GoodNoGap) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(replaced=2)
type Foo = struct {};

@available(added=2)
type Foo = table {};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.HasStruct("Foo"), GetParam() == V1);
  EXPECT_EQ(library.HasTable("Foo"), GetParam() >= V2);
}

TEST_P(VersioningOverlapTest, GoodWithGap) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(removed=2)
type Foo = struct {};

@available(added=3)
type Foo = table {};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.HasStruct("Foo"), GetParam() == V1);
  EXPECT_EQ(library.HasTable("Foo"), GetParam() >= V3);
}

TEST_P(VersioningOverlapTest, GoodNoGapCanonical) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(removed=2)
type foo = struct {};

@available(added=2)
type FOO = table {};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.HasStruct("foo"), GetParam() == V1);
  EXPECT_EQ(library.HasTable("FOO"), GetParam() >= V2);
}

TEST_P(VersioningOverlapTest, GoodWithGapCanonical) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(removed=2)
type foo = struct {};

@available(added=3)
type FOO = table {};
)FIDL");
  library.SelectVersion("example", GetParam());
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.HasStruct("foo"), GetParam() == V1);
  EXPECT_EQ(library.HasTable("FOO"), GetParam() >= V3);
}

TEST_P(VersioningOverlapTest, BadEqual) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {};
type Foo = table {};
)FIDL");
  library.SelectVersion("example", GetParam());
  library.ExpectFail(ErrNameCollision, Element::Kind::kTable, "Foo", Element::Kind::kStruct,
                     "example.fidl:5:6");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningOverlapTest, BadEqualLegacy) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(removed=2, legacy=true)
type Foo = struct {};
@available(removed=2, legacy=true)
type Foo = table {};
)FIDL");
  library.SelectVersion("example", GetParam());
  library.ExpectFail(ErrNameCollision, Element::Kind::kTable, "Foo", Element::Kind::kStruct,
                     "example.fidl:6:6");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningOverlapTest, BadEqualCanonical) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type foo = struct {};
type FOO = table {};
)FIDL");
  library.SelectVersion("example", GetParam());
  library.ExpectFail(ErrNameCollisionCanonical, Element::Kind::kStruct, "foo",
                     Element::Kind::kTable, "FOO", "example.fidl:6:6", "foo");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningOverlapTest, BadExample) {
  TestLibrary library;
  library.AddFile("bad/fi-0036.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(ErrNameOverlap, Element::Kind::kEnum, "Color", Element::Kind::kEnum,
                     "bad/fi-0036.test.fidl:7:6",
                     VersionSet(VersionRange(Version::From(2).value(), Version::PosInf())),
                     Platform::Parse("test").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningOverlapTest, BadExampleCanonical) {
  TestLibrary library;
  library.AddFile("bad/fi-0037.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(ErrNameOverlapCanonical, Element::Kind::kProtocol, "Color",
                     Element::Kind::kConst, "COLOR", "bad/fi-0037.test.fidl:7:7", "color",
                     VersionSet(VersionRange(Version::From(2).value(), Version::PosInf())),
                     Platform::Parse("test").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningOverlapTest, BadSubset) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {};
@available(removed=2)
type Foo = table {};
)FIDL");
  library.SelectVersion("example", GetParam());
  library.ExpectFail(ErrNameOverlap, Element::Kind::kTable, "Foo", Element::Kind::kStruct,
                     "example.fidl:5:6",
                     VersionSet(VersionRange(Version::From(1).value(), Version::From(2).value())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningOverlapTest, BadSubsetCanonical) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type foo = struct {};
@available(removed=2)
type FOO = table {};
)FIDL");
  library.SelectVersion("example", GetParam());
  library.ExpectFail(ErrNameOverlapCanonical, Element::Kind::kStruct, "foo", Element::Kind::kTable,
                     "FOO", "example.fidl:7:6", "foo",
                     VersionSet(VersionRange(Version::From(1).value(), Version::From(2).value())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningOverlapTest, BadIntersect) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(removed=5)
type Foo = struct {};
@available(added=3)
type Foo = table {};
)FIDL");
  library.SelectVersion("example", GetParam());
  library.ExpectFail(ErrNameOverlap, Element::Kind::kTable, "Foo", Element::Kind::kStruct,
                     "example.fidl:6:6",
                     VersionSet(VersionRange(Version::From(3).value(), Version::From(5).value())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningOverlapTest, BadIntersectCanonical) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(removed=5)
type foo = struct {};
@available(added=3)
type FOO = table {};
)FIDL");
  library.SelectVersion("example", GetParam());
  library.ExpectFail(ErrNameOverlapCanonical, Element::Kind::kStruct, "foo", Element::Kind::kTable,
                     "FOO", "example.fidl:8:6", "foo",
                     VersionSet(VersionRange(Version::From(3).value(), Version::From(5).value())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningOverlapTest, BadJustLegacy) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(replaced=2, legacy=true)
type Foo = struct {};
@available(added=2, removed=3, legacy=true)
type Foo = table {};
)FIDL");
  library.SelectVersion("example", GetParam());
  library.ExpectFail(ErrNameOverlap, Element::Kind::kTable, "Foo", Element::Kind::kStruct,
                     "example.fidl:6:6",
                     VersionSet(VersionRange(Version::Legacy(), Version::PosInf())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningOverlapTest, BadJustLegacyCanonical) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(removed=2, legacy=true)
type foo = struct {};
@available(added=2, removed=3, legacy=true)
type FOO = table {};
)FIDL");
  library.SelectVersion("example", GetParam());
  library.ExpectFail(ErrNameOverlapCanonical, Element::Kind::kStruct, "foo", Element::Kind::kTable,
                     "FOO", "example.fidl:8:6", "foo",
                     VersionSet(VersionRange(Version::Legacy(), Version::PosInf())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningOverlapTest, BadIntersectLegacy) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(replaced=2, legacy=true)
type Foo = struct {};
@available(added=2)
type Foo = table {};
)FIDL");
  library.SelectVersion("example", GetParam());
  library.ExpectFail(ErrNameOverlap, Element::Kind::kTable, "Foo", Element::Kind::kStruct,
                     "example.fidl:6:6",
                     VersionSet(VersionRange(Version::Legacy(), Version::PosInf())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningOverlapTest, BadIntersectLegacyCanonical) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(removed=2, legacy=true)
type foo = struct {};
@available(added=2)
type FOO = table {};
)FIDL");
  library.SelectVersion("example", GetParam());
  library.ExpectFail(ErrNameOverlapCanonical, Element::Kind::kStruct, "foo", Element::Kind::kTable,
                     "FOO", "example.fidl:8:6", "foo",
                     VersionSet(VersionRange(Version::Legacy(), Version::PosInf())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningOverlapTest, BadMultiple) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {};
@available(added=3)
type Foo = table {};
@available(added=HEAD)
const Foo uint32 = 0;
)FIDL");
  library.SelectVersion("example", GetParam());
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

TEST_P(VersioningOverlapTest, BadRecursive) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=1, removed=5)
type Foo = struct { member box<Foo>; };

@available(added=3, removed=7)
type Foo = struct { member box<Foo>; };
)FIDL");
  library.SelectVersion("example", GetParam());
  library.ExpectFail(ErrNameOverlap, Element::Kind::kStruct, "Foo", Element::Kind::kStruct,
                     "example.fidl:6:6",
                     VersionSet(VersionRange(Version::From(3).value(), Version::From(5).value())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningOverlapTest, BadMemberEqual) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    member bool;
    member bool;
};
)FIDL");
  library.SelectVersion("example", GetParam());
  library.ExpectFail(ErrNameCollision, Element::Kind::kStructMember, "member",
                     Element::Kind::kStructMember, "example.fidl:6:5");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningOverlapTest, BadMemberEqualLegacy) {
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
  library.SelectVersion("example", GetParam());
  library.ExpectFail(ErrNameCollision, Element::Kind::kStructMember, "member",
                     Element::Kind::kStructMember, "example.fidl:7:5");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningOverlapTest, BadMemberEqualCanonical) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    member bool;
    MEMBER bool;
};
)FIDL");
  library.SelectVersion("example", GetParam());
  library.ExpectFail(ErrNameCollisionCanonical, Element::Kind::kStructMember, "MEMBER",
                     Element::Kind::kStructMember, "member", "example.fidl:6:5", "member");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningOverlapTest, BadMemberSubset) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    member bool;
    @available(removed=2)
    member bool;
};
)FIDL");
  library.SelectVersion("example", GetParam());
  library.ExpectFail(ErrNameOverlap, Element::Kind::kStructMember, "member",
                     Element::Kind::kStructMember, "example.fidl:6:5",
                     VersionSet(VersionRange(Version::From(1).value(), Version::From(2).value())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningOverlapTest, BadMemberSubsetCanonical) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    member bool;
    @available(removed=2)
    MEMBER bool;
};
)FIDL");
  library.SelectVersion("example", GetParam());
  library.ExpectFail(ErrNameOverlapCanonical, Element::Kind::kStructMember, "MEMBER",
                     Element::Kind::kStructMember, "member", "example.fidl:6:5", "member",
                     VersionSet(VersionRange(Version::From(1).value(), Version::From(2).value())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningOverlapTest, BadMemberIntersect) {
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
  library.SelectVersion("example", GetParam());
  library.ExpectFail(ErrNameOverlap, Element::Kind::kStructMember, "member",
                     Element::Kind::kStructMember, "example.fidl:7:5",
                     VersionSet(VersionRange(Version::From(3).value(), Version::From(5).value())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningOverlapTest, BadMemberIntersectCanonical) {
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
  library.SelectVersion("example", GetParam());
  library.ExpectFail(ErrNameOverlapCanonical, Element::Kind::kStructMember, "MEMBER",
                     Element::Kind::kStructMember, "member", "example.fidl:7:5", "member",
                     VersionSet(VersionRange(Version::From(3).value(), Version::From(5).value())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningOverlapTest, BadMemberJustLegacy) {
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
  library.SelectVersion("example", GetParam());
  library.ExpectFail(ErrNameOverlap, Element::Kind::kStructMember, "member",
                     Element::Kind::kStructMember, "example.fidl:7:5",
                     VersionSet(VersionRange(Version::Legacy(), Version::PosInf())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningOverlapTest, BadMemberJustLegacyCanonical) {
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
  library.SelectVersion("example", GetParam());
  library.ExpectFail(ErrNameOverlapCanonical, Element::Kind::kStructMember, "MEMBER",
                     Element::Kind::kStructMember, "member", "example.fidl:7:5", "member",
                     VersionSet(VersionRange(Version::Legacy(), Version::PosInf())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningOverlapTest, BadMemberIntersectLegacy) {
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
  library.SelectVersion("example", GetParam());
  library.ExpectFail(ErrNameOverlap, Element::Kind::kStructMember, "member",
                     Element::Kind::kStructMember, "example.fidl:7:5",
                     VersionSet(VersionRange(Version::Legacy(), Version::PosInf())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningOverlapTest, BadMemberIntersectLegacyCanonical) {
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
  library.SelectVersion("example", GetParam());
  library.ExpectFail(ErrNameOverlapCanonical, Element::Kind::kStructMember, "MEMBER",
                     Element::Kind::kStructMember, "member", "example.fidl:7:5", "member",
                     VersionSet(VersionRange(Version::Legacy(), Version::PosInf())),
                     Platform::Parse("example").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST_P(VersioningOverlapTest, BadMemberMultiple) {
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
  library.SelectVersion("example", GetParam());
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

TEST_P(VersioningOverlapTest, BadMemberMultipleCanonical) {
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
  library.SelectVersion("example", GetParam());
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

}  // namespace
}  // namespace fidlc
