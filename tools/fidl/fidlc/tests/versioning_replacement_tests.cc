// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "tools/fidl/fidlc/tests/test_library.h"

// This file tests how FIDL versioning distinguishes "removal" from
// "replacement", and validation that replacement is done correctly.

namespace fidlc {
namespace {

TEST(VersioningReplacementTests, BadRemovedWithReplacement) {
  TestLibrary library;
  library.AddFile("bad/fi-0205.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(ErrRemovedWithReplacement, "Bar", Version::From(2).value(),
                     "bad/fi-0205.test.fidl:11:14");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningReplacementTests, BadRemovedNamedToAnonymous) {
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

TEST(VersioningReplacementTests, GoodRemovedAnonymousToNamed) {
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

TEST(VersioningReplacementTests, GoodRemovedAnonymousToAnonymous) {
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

TEST(VersioningReplacementTests, BadReplacedWithoutReplacement) {
  TestLibrary library;
  library.AddFile("bad/fi-0206.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(ErrReplacedWithoutReplacement, "Bar", Version::From(2).value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningReplacementTests, GoodAnonymousReplacedWithoutReplacement) {
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

TEST(VersioningReplacementTests, GoodReplacedNamedToAnonymous) {
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

TEST(VersioningReplacementTests, GoodReplacedAnonymousToNamed) {
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

TEST(VersioningReplacementTests, GoodReplacedAnonymousToAnonymous) {
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

TEST(VersioningReplacementTests, GoodReplacedTwice) {
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

}  // namespace
}  // namespace fidlc
