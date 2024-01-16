// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "tools/fidl/fidlc/include/fidl/diagnostics.h"
#include "tools/fidl/fidlc/tests/test_library.h"

namespace {

TEST(EnumsTests, GoodEnumTestSimple) {
  TestLibrary library(R"FIDL(library example;

type Fruit = enum : uint64 {
    ORANGE = 1;
    APPLE = 2;
    BANANA = 3;
};
)FIDL");
  ASSERT_COMPILED(library);
  auto type_decl = library.LookupEnum("Fruit");
  ASSERT_NE(type_decl, nullptr);
  EXPECT_EQ(type_decl->members.size(), 3u);
  auto underlying = type_decl->subtype_ctor->type;
  ASSERT_EQ(underlying->kind, fidlc::Type::Kind::kPrimitive);
  auto underlying_primitive = static_cast<const fidlc::PrimitiveType*>(underlying);
  EXPECT_EQ(underlying_primitive->subtype, fidlc::PrimitiveSubtype::kUint64);
}

TEST(BitsTests, GoodEnumDefaultUint32) {
  TestLibrary library(R"FIDL(library example;

type Fruit = enum {
    ORANGE = 1;
};
)FIDL");
  ASSERT_COMPILED(library);
  auto type_decl = library.LookupEnum("Fruit");
  ASSERT_NE(type_decl, nullptr);
  auto underlying = type_decl->subtype_ctor->type;
  ASSERT_EQ(underlying->kind, fidlc::Type::Kind::kPrimitive);
  auto underlying_primitive = static_cast<const fidlc::PrimitiveType*>(underlying);
  EXPECT_EQ(underlying_primitive->subtype, fidlc::PrimitiveSubtype::kUint32);
}

TEST(EnumsTests, BadEnumTestWithNonUniqueValues) {
  TestLibrary library;
  library.AddFile("bad/fi-0107.test.fidl");
  library.ExpectFail(fidlc::ErrDuplicateMemberValue, fidlc::Decl::Kind::kEnum, "APPLE", "ORANGE",
                     "bad/fi-0107.test.fidl:7:5");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(EnumsTests, BadEnumTestWithNonUniqueValuesOutOfLine) {
  TestLibrary library(R"FIDL(
library example;

type Fruit = enum {
    ORANGE = FOUR;
    APPLE = TWO_SQUARED;
};

const FOUR uint32 = 4;
const TWO_SQUARED uint32 = 4;
)FIDL");
  library.ExpectFail(fidlc::ErrDuplicateMemberValue, fidlc::Decl::Kind::kEnum, "APPLE", "ORANGE",
                     "example.fidl:5:5");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(EnumsTests, BadEnumTestUnsignedWithNegativeMember) {
  TestLibrary library(R"FIDL(
library example;

type Fruit = enum : uint64 {
    ORANGE = 1;
    APPLE = -2;
};
)FIDL");
  library.ExpectFail(fidlc::ErrCouldNotResolveMember, fidlc::Decl::Kind::kEnum);
  library.ExpectFail(fidlc::ErrConstantOverflowsType, "-2", "uint64");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(EnumsTests, BadEnumTestInferredUnsignedWithNegativeMember) {
  TestLibrary library(R"FIDL(
library example;

type Fruit = enum {
    ORANGE = 1;
    APPLE = -2;
};
)FIDL");
  library.ExpectFail(fidlc::ErrCouldNotResolveMember, fidlc::Decl::Kind::kEnum);
  library.ExpectFail(fidlc::ErrConstantOverflowsType, "-2", "uint32");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(EnumsTests, BadEnumTestMemberOverflow) {
  TestLibrary library(R"FIDL(
library example;

type Fruit = enum : uint8 {
    ORANGE = 1;
    APPLE = 256;
};
)FIDL");
  library.ExpectFail(fidlc::ErrCouldNotResolveMember, fidlc::Decl::Kind::kEnum);
  library.ExpectFail(fidlc::ErrConstantOverflowsType, "256", "uint8");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(EnumsTests, BadEnumTestFloatType) {
  TestLibrary library;
  library.AddFile("bad/fi-0070.test.fidl");
  library.ExpectFail(fidlc::ErrEnumTypeMustBeIntegralPrimitive, "float64");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(EnumsTests, BadEnumTestDuplicateMember) {
  TestLibrary library(R"FIDL(
library example;

type Fruit = flexible enum {
    ORANGE = 1;
    APPLE = 2;
    ORANGE = 3;
};
)FIDL");
  library.ExpectFail(fidlc::ErrNameCollision, fidlc::Element::Kind::kEnumMember, "ORANGE",
                     fidlc::Element::Kind::kEnumMember, "example.fidl:5:5");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(EnumsTests, GoodEnumTestNoMembersAllowedWhenDefaultsToFlexible) {
  TestLibrary library(R"FIDL(
library example;

type E = enum {};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(EnumsTests, GoodEnumTestNoMembersAllowedWhenFlexible) {
  TestLibrary library;
  library.AddFile("good/fi-0019-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(EnumsTests, GoodEnumTestStrictWithMembers) {
  TestLibrary library;
  library.AddFile("good/fi-0019-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(EnumsTests, BadEnumTestNoMembersWhenStrict) {
  TestLibrary library;
  library.AddFile("bad/fi-0019.test.fidl");
  library.ExpectFail(fidlc::ErrMustHaveOneMember);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(EnumsTests, GoodEnumTestKeywordNames) {
  TestLibrary library(R"FIDL(library example;

type Fruit = enum : uint64 {
    library = 1;
    enum = 2;
    uint64 = 3;
};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(EnumsTests, BadEnumShantBeNullable) {
  TestLibrary library(R"FIDL(
library example;

type NotNullable = enum {
    MEMBER = 1;
};

type Struct = struct {
    not_nullable NotNullable:optional;
};
)FIDL");
  library.ExpectFail(fidlc::ErrCannotBeOptional, "NotNullable");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(EnumsTests, BadEnumMultipleConstraints) {
  TestLibrary library(R"FIDL(
library example;

type NotNullable = enum {
    MEMBER = 1;
};

type Struct = struct {
    not_nullable NotNullable:<1, 2, 3>;
};
)FIDL");
  library.ExpectFail(fidlc::ErrTooManyConstraints, "NotNullable", 1, 3);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(EnumsTests, GoodSimpleEnum) {
  TestLibrary library;
  library.AddFile("good/fi-0008.test.fidl");
  ASSERT_COMPILED(library);
}

}  // namespace
