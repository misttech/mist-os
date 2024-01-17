// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "tools/fidl/fidlc/src/flat_ast.h"
#include "tools/fidl/fidlc/src/source_file.h"
#include "tools/fidl/fidlc/tests/test_library.h"

namespace {

TEST(StrictnessTests, BadDuplicateModifier) {
  TestLibrary library(R"FIDL(
library example;

type One = strict union { 1: b bool; };
type Two = strict strict union { 1: b bool; };
type Three = strict strict strict union { 1: b bool; };
)FIDL");
  library.ExpectFail(fidlc::ErrDuplicateModifier,
                     fidlc::Token::KindAndSubkind(fidlc::Token::Subkind::kStrict));
  library.ExpectFail(fidlc::ErrDuplicateModifier,
                     fidlc::Token::KindAndSubkind(fidlc::Token::Subkind::kStrict));
  library.ExpectFail(fidlc::ErrDuplicateModifier,
                     fidlc::Token::KindAndSubkind(fidlc::Token::Subkind::kStrict));
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(StrictnessTests, BadDuplicateModifierNonConsecutive) {
  TestLibrary library;
  library.AddFile("bad/fi-0032.test.fidl");
  library.ExpectFail(fidlc::ErrDuplicateModifier,
                     fidlc::Token::KindAndSubkind(fidlc::Token::Subkind::kStrict));
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(StrictnessTests, BadConflictingModifiers) {
  TestLibrary library;
  library.AddFile("bad/fi-0033.test.fidl");
  library.ExpectFail(fidlc::ErrConflictingModifier,
                     fidlc::Token::KindAndSubkind(fidlc::Token::Subkind::kFlexible),
                     fidlc::Token::KindAndSubkind(fidlc::Token::Subkind::kStrict));
  library.ExpectFail(fidlc::ErrConflictingModifier,
                     fidlc::Token::KindAndSubkind(fidlc::Token::Subkind::kStrict),
                     fidlc::Token::KindAndSubkind(fidlc::Token::Subkind::kFlexible));
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(StrictnessTests, GoodBitsStrictness) {
  TestLibrary library(
      R"FIDL(library example;

type DefaultStrictFoo = strict bits {
    BAR = 0x1;
};

type StrictFoo = strict bits {
    BAR = 0x1;
};

type FlexibleFoo = flexible bits {
    BAR = 0x1;
};
)FIDL");
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.LookupBits("FlexibleFoo")->strictness, fidlc::Strictness::kFlexible);
  EXPECT_EQ(library.LookupBits("StrictFoo")->strictness, fidlc::Strictness::kStrict);
  EXPECT_EQ(library.LookupBits("DefaultStrictFoo")->strictness, fidlc::Strictness::kStrict);
}

TEST(StrictnessTests, GoodEnumStrictness) {
  TestLibrary library(
      R"FIDL(library example;

type DefaultStrictFoo = strict enum {
    BAR = 1;
};

type StrictFoo = strict enum {
    BAR = 1;
};

type FlexibleFoo = flexible enum {
    BAR = 1;
};
)FIDL");
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.LookupEnum("FlexibleFoo")->strictness, fidlc::Strictness::kFlexible);
  EXPECT_EQ(library.LookupEnum("StrictFoo")->strictness, fidlc::Strictness::kStrict);
  EXPECT_EQ(library.LookupEnum("DefaultStrictFoo")->strictness, fidlc::Strictness::kStrict);
}

TEST(StrictnessTests, GoodFlexibleEnum) {
  TestLibrary library(R"FIDL(library example;

type Foo = flexible enum {
    BAR = 1;
};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(StrictnessTests, GoodFlexibleBitsRedundant) {
  TestLibrary library(R"FIDL(library example;

type Foo = flexible bits {
    BAR = 0x1;
};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(StrictnessTests, BadStrictnessStruct) {
  TestLibrary library;
  library.AddFile("bad/fi-0030.test.fidl");
  library.ExpectFail(fidlc::ErrCannotSpecifyModifier,
                     fidlc::Token::KindAndSubkind(fidlc::Token::Subkind::kStrict),
                     fidlc::Token::KindAndSubkind(fidlc::Token::Subkind::kStruct));
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(StrictnessTests, BadStrictnessTable) {
  TestLibrary library(R"FIDL(
library example;

type StrictFoo = strict table {};
)FIDL");
  library.ExpectFail(fidlc::ErrCannotSpecifyModifier,
                     fidlc::Token::KindAndSubkind(fidlc::Token::Subkind::kStrict),
                     fidlc::Token::KindAndSubkind(fidlc::Token::Subkind::kTable));
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(StrictnessTests, GoodUnionStrictness) {
  TestLibrary library;
  library.AddFile("good/fi-0033.test.fidl");

  ASSERT_COMPILED(library);
  EXPECT_EQ(library.LookupUnion("FlexibleFoo")->strictness, fidlc::Strictness::kFlexible);
  EXPECT_EQ(library.LookupUnion("StrictBar")->strictness, fidlc::Strictness::kStrict);
}

TEST(StrictnessTests, GoodStrictUnionRedundant) {
  TestLibrary library(R"FIDL(library example;

type Foo = strict union {
    1: i int32;
};
)FIDL");
  ASSERT_COMPILED(library);
  ASSERT_EQ(library.LookupUnion("Foo")->strictness, fidlc::Strictness::kStrict);
}

}  // namespace
