// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zxtest/zxtest.h>

#include "tools/fidl/fidlc/include/fidl/flat_ast.h"
#include "tools/fidl/fidlc/tests/test_library.h"

namespace {

TEST(ResourceTests, GoodValidWithoutRights) {
  TestLibrary library(R"FIDL(library example;

type MyEnum = strict enum : uint32 {
    NONE = 0;
};

resource_definition SomeResource : uint32 {
    properties {
        subtype MyEnum;
    };
};
)FIDL");
  ASSERT_COMPILED(library);

  auto resource = library.LookupResource("SomeResource");
  ASSERT_NOT_NULL(resource);
  ASSERT_EQ(resource->properties.size(), 1u);

  ASSERT_NOT_NULL(resource->subtype_ctor);
  auto underlying = resource->subtype_ctor->type;
  ASSERT_EQ(underlying->kind, fidl::flat::Type::Kind::kPrimitive);
  auto underlying_primitive = static_cast<const fidl::flat::PrimitiveType*>(underlying);
  EXPECT_EQ(underlying_primitive->subtype, fidl::types::PrimitiveSubtype::kUint32);

  auto& subtype = resource->properties[0];
  EXPECT_EQ(subtype.name.data(), "subtype");
  EXPECT_EQ(resource->properties[0].type_ctor->layout.resolved().element(),
            library.LookupEnum("MyEnum"));
}

TEST(ResourceTests, GoodValidWithRights) {
  TestLibrary library(R"FIDL(library example;

type MyEnum = strict enum : uint32 {
    NONE = 0;
};

resource_definition SomeResource : uint32 {
    properties {
        subtype MyEnum;
        rights uint32;
    };
};
)FIDL");
  ASSERT_COMPILED(library);

  auto resource = library.LookupResource("SomeResource");
  ASSERT_NOT_NULL(resource);
  ASSERT_EQ(resource->properties.size(), 2u);

  ASSERT_NOT_NULL(resource->subtype_ctor);
  auto underlying = resource->subtype_ctor->type;
  ASSERT_EQ(underlying->kind, fidl::flat::Type::Kind::kPrimitive);
  auto underlying_primitive = static_cast<const fidl::flat::PrimitiveType*>(underlying);
  EXPECT_EQ(underlying_primitive->subtype, fidl::types::PrimitiveSubtype::kUint32);

  auto& subtype = resource->properties[0];
  EXPECT_EQ(subtype.name.data(), "subtype");
  EXPECT_EQ(resource->properties[0].type_ctor->layout.resolved().element(),
            library.LookupEnum("MyEnum"));

  auto& rights = resource->properties[1];
  EXPECT_EQ(rights.name.data(), "rights");
  EXPECT_EQ(rights.type_ctor->type->kind, fidl::flat::Type::Kind::kPrimitive);
  EXPECT_EQ(static_cast<const fidl::flat::PrimitiveType*>(rights.type_ctor->type)->subtype,
            fidl::types::PrimitiveSubtype::kUint32);
}

TEST(ResourceTests, GoodAliasedBaseTypeWithoutRights) {
  TestLibrary library(R"FIDL(library example;

type MyEnum = strict enum : uint32 {
    NONE = 0;
};

alias via = uint32;

resource_definition SomeResource : via {
    properties {
        subtype MyEnum;
    };
};
)FIDL");
  ASSERT_COMPILED(library);

  auto resource = library.LookupResource("SomeResource");
  ASSERT_NOT_NULL(resource);
  ASSERT_EQ(resource->properties.size(), 1u);

  ASSERT_NOT_NULL(resource->subtype_ctor);
  auto underlying = resource->subtype_ctor->type;
  ASSERT_EQ(underlying->kind, fidl::flat::Type::Kind::kPrimitive);
  auto underlying_primitive = static_cast<const fidl::flat::PrimitiveType*>(underlying);
  EXPECT_EQ(underlying_primitive->subtype, fidl::types::PrimitiveSubtype::kUint32);

  auto& subtype = resource->properties[0];
  EXPECT_EQ(subtype.name.data(), "subtype");
  EXPECT_EQ(resource->properties[0].type_ctor->layout.resolved().element(),
            library.LookupEnum("MyEnum"));
}

TEST(ResourceTests, GoodAliasedBaseTypeWithRights) {
  TestLibrary library(R"FIDL(library example;

type MyEnum = strict enum : uint32 {
    NONE = 0;
};

alias via = uint32;

resource_definition SomeResource : via {
    properties {
        subtype MyEnum;
        rights via;
    };
};
)FIDL");
  ASSERT_COMPILED(library);

  auto resource = library.LookupResource("SomeResource");
  ASSERT_NOT_NULL(resource);
  ASSERT_EQ(resource->properties.size(), 2u);

  ASSERT_NOT_NULL(resource->subtype_ctor);
  auto underlying = resource->subtype_ctor->type;
  ASSERT_EQ(underlying->kind, fidl::flat::Type::Kind::kPrimitive);
  auto underlying_primitive = static_cast<const fidl::flat::PrimitiveType*>(underlying);
  EXPECT_EQ(underlying_primitive->subtype, fidl::types::PrimitiveSubtype::kUint32);

  auto& subtype = resource->properties[0];
  EXPECT_EQ(subtype.name.data(), "subtype");
  EXPECT_EQ(resource->properties[0].type_ctor->layout.resolved().element(),
            library.LookupEnum("MyEnum"));

  auto& rights = resource->properties[1];
  EXPECT_EQ(rights.name.data(), "rights");
  EXPECT_EQ(rights.type_ctor->type->kind, fidl::flat::Type::Kind::kPrimitive);
  EXPECT_EQ(static_cast<const fidl::flat::PrimitiveType*>(rights.type_ctor->type)->subtype,
            fidl::types::PrimitiveSubtype::kUint32);
}

TEST(ResourceTests, BadEmpty) {
  TestLibrary library(R"FIDL(
library example;

resource_definition SomeResource : uint32 {
};

)FIDL");
  library.ExpectFail(fidl::ErrUnexpectedIdentifier,
                     fidl::Token::KindAndSubkind(fidl::Token::Kind::kRightCurly),
                     fidl::Token::KindAndSubkind(fidl::Token::Subkind::kProperties));
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ResourceTests, BadNoProperties) {
  TestLibrary library;
  library.AddFile("bad/fi-0029.test.fidl");
  library.ExpectFail(fidl::ErrMustHaveOneProperty);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ResourceTests, BadDuplicateProperty) {
  TestLibrary library(R"FIDL(
library example;

resource_definition MyResource : uint32 {
    properties {
        subtype flexible enum : uint32 {};
        rights uint32;
        rights uint32;
    };
};
)FIDL");
  library.ExpectFail(fidl::ErrNameCollision, fidl::flat::Element::Kind::kResourceProperty, "rights",
                     fidl::flat::Element::Kind::kResourceProperty, "example.fidl:7:9");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ResourceTests, BadNotUint32) {
  TestLibrary library;
  library.AddFile("bad/fi-0172.test.fidl");
  library.ExpectFail(fidl::ErrResourceMustBeUint32Derived, "MyResource");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ResourceTests, BadMissingSubtypePropertyTest) {
  TestLibrary library;
  library.AddFile("bad/fi-0173.test.fidl");
  library.ExpectFail(fidl::ErrResourceMissingSubtypeProperty, "MyResource");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ResourceTests, BadSubtypeNotEnum) {
  TestLibrary library;
  library.AddFile("bad/fi-0175.test.fidl");
  library.ExpectFail(fidl::ErrResourceSubtypePropertyMustReferToEnum, "MyResource");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ResourceTests, BadSubtypeNotIdentifier) {
  TestLibrary library(R"FIDL(
library example;

resource_definition handle : uint32 {
    properties {
        subtype uint32;
    };
};
)FIDL");
  library.ExpectFail(fidl::ErrResourceSubtypePropertyMustReferToEnum, "handle");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ResourceTests, BadNonBitsRights) {
  TestLibrary library;
  library.AddFile("bad/fi-0177.test.fidl");
  library.ExpectFail(fidl::ErrResourceRightsPropertyMustReferToBits, "MyResource");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ResourceTests, BadIncludeCycle) {
  TestLibrary library(R"FIDL(
library example;

resource_definition handle : uint32 {
    properties {
        subtype handle;
    };
};
)FIDL");
  library.ExpectFail(fidl::ErrIncludeCycle, "resource 'handle' -> resource 'handle'");
  library.ExpectFail(fidl::ErrResourceSubtypePropertyMustReferToEnum, "handle");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

}  // namespace
