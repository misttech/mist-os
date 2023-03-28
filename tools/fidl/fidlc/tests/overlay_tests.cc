// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zxtest/zxtest.h>

#include "tools/fidl/fidlc/include/fidl/flat_ast.h"
#include "tools/fidl/fidlc/tests/error_test.h"
#include "tools/fidl/fidlc/tests/test_library.h"

TEST(OverlayTests, GoodOverlayInOtherLayouts) {
  TestLibrary library(R"FIDL(library test;

type Overlay = strict overlay {
    1: member string:32;
};

type Struct = struct {
    o Overlay;
};

type Table = table {
    1: o Overlay;
};

type Union = strict union {
    1: o Overlay;
};
)FIDL");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kZxCTypes);

  ASSERT_COMPILED(library);
}

TEST(OverlayTests, GoodOtherLayoutsInOverlay) {
  TestLibrary library(R"FIDL(library test;

type Struct = struct {
    member int32;
};

type Table = table {
    1: member int32;
};

type Union = strict union {
    1: member int32;
};

type Overlay = strict overlay {
    1: s Struct;
    2: bs box<Struct>;
    3: t Table;
    4: u Union;
};


)FIDL");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kZxCTypes);

  ASSERT_COMPILED(library);
}

TEST(OverlayTests, GoodOverlayInOverlay) {
  TestLibrary library(R"FIDL(library test;

type Inner = strict overlay {
    1: i int32;
    2: b bool;
    3: s string:32;
};

type Outer = strict overlay {
    1: i int32;
    2: b bool;
    3: s string:32;
    4: o Inner;
};


)FIDL");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kZxCTypes);

  ASSERT_COMPILED(library);
}

TEST(OverlayTests, GoodKeywordsAsFieldNames) {
  TestLibrary library(R"FIDL(library test;

type struct = struct {
    field bool;
};

type Foo = strict overlay {
    1: union int64;
    2: library bool;
    3: uint32 uint32;
    4: member struct;
    5: reserved bool;
};
)FIDL");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kZxCTypes);

  ASSERT_COMPILED(library);
  auto type_decl = library.LookupOverlay("Foo");
  ASSERT_NOT_NULL(type_decl);
  EXPECT_EQ(type_decl->members.size(), 5);
}

TEST(OverlayTests, BadFlexible) {
  TestLibrary library(R"FIDL(library test;

type Foo = flexible overlay {
    1: flippity int64;
    2: floppity bool;
};

)FIDL");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kZxCTypes);

  ASSERT_ERRORED_DURING_COMPILE(library, fidl::ErrOverlayMustBeStrict);
}

TEST(OverlayTests, BadResource) {
  TestLibrary library(R"FIDL(library test;

type Foo = strict resource overlay {
    1: flippity int64;
    2: floppity bool;
};

)FIDL");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kZxCTypes);

  ASSERT_ERRORED_DURING_COMPILE(library, fidl::ErrCannotSpecifyModifier);
}

TEST(OverlayTests, BadResourceMember) {
  TestLibrary library(R"FIDL(library test;
protocol Bar{};

type Foo = strict overlay {
    1: flippity int64;
    2: floppity client_end:Bar;
};

)FIDL");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kZxCTypes);

  ASSERT_ERRORED_DURING_COMPILE(library, fidl::ErrOverlayMemberMustBeValue);
}

TEST(OverlayTests, BadReserved) {
  TestLibrary library(R"FIDL(library test;

type Foo = strict overlay {
    1: flippity int64;
    2: floppity float64;
    3: reserved;
};

)FIDL");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kZxCTypes);

  ASSERT_ERRORED_DURING_COMPILE(library, fidl::ErrOverlayMustNotContainReserved);
}

TEST(OverlayTests, BadNoExperimentalFlag) {
  TestLibrary library(R"FIDL(library test;

type Foo = strict overlay {
    1: bar int64;
    2: baz string:42;
};

)FIDL");
  ASSERT_ERRORED_DURING_COMPILE(library, fidl::ErrInvalidLayoutClass);
}

TEST(OverlayTests, BadOptionalOverlay) {
  TestLibrary library(R"FIDL(library test;

type Biff = strict overlay {
    1: foo bool;
    2: bar string;
};

type Baff = struct {
    baz Biff:optional;
};
)FIDL");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kZxCTypes);

  ASSERT_ERRORED_DURING_COMPILE(library, fidl::ErrCannotBeOptional);
}

TEST(OverlayTests, GoodRecursiveOverlay) {
  TestLibrary library(R"FIDL(library test;

type Foo = strict union {
    1: bar Bar;
    2: s string:32;
    3: i int32;
};

type Bar = strict overlay {
    1: foo Foo:optional;
    2: s string:32;
    3: i int32;
};

)FIDL");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kZxCTypes);

  ASSERT_COMPILED(library);
}

TEST(OverlayTests, BadDirectlyRecursiveOverlay) {
  TestLibrary library(R"FIDL(library test;

type Value = strict overlay {
    1: bool_value bool;
    2: recurse Value;
};
)FIDL");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kZxCTypes);

  ASSERT_ERRORED_DURING_COMPILE(library, fidl::ErrIncludeCycle);
}

TEST(OverlayTests, BadInlineRecursiveOverlay) {
  TestLibrary library(R"FIDL(library test;

type Product = struct {
    i int32;
    b bool;
    s string:32;
    sum Sum;
};

type Sum = strict overlay {
    1: i int32;
    2: b bool;
    3: s string:32;
    4: product Product;
};
)FIDL");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kZxCTypes);

  ASSERT_ERRORED_DURING_COMPILE(library, fidl::ErrIncludeCycle);
}

TEST(OverlayTests, BadNoSelector) {
  TestLibrary library(R"FIDL(
library example;

type Foo = strict overlay {
  @selector("v2") 1: v string;
};

)FIDL");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kZxCTypes);

  ASSERT_ERRORED_DURING_COMPILE(library, fidl::ErrInvalidAttributePlacement);
}
