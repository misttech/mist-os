// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <locale.h>

#include <gtest/gtest.h>

#include "tools/fidl/fidlc/include/fidl/diagnostics.h"
#include "tools/fidl/fidlc/include/fidl/flat_ast.h"
#include "tools/fidl/fidlc/tests/test_library.h"

namespace {

TEST(HandleTests, GoodHandleRightsTest) {
  TestLibrary library(R"FIDL(
library example;

using zx;

type MyStruct = resource struct {
    h zx.Handle:<THREAD, zx.Rights.DUPLICATE | zx.Rights.TRANSFER>;
};
)FIDL");
  library.UseLibraryZx();
  ASSERT_COMPILED(library);

  const auto& h_type_ctor = library.LookupStruct("MyStruct")->members[0].type_ctor;

  ASSERT_TRUE(h_type_ctor->resolved_params.subtype_raw != nullptr);
  EXPECT_EQ("THREAD", h_type_ctor->resolved_params.subtype_raw->span.data());

  auto h_type = h_type_ctor->type;
  ASSERT_NE(h_type, nullptr);
  ASSERT_EQ(h_type->kind, fidl::flat::Type::Kind::kHandle);
  auto handle_type = static_cast<const fidl::flat::HandleType*>(h_type);

  EXPECT_EQ(fidl::types::HandleSubtype::kThread, handle_type->subtype);
  EXPECT_EQ(
      static_cast<const fidl::flat::NumericConstantValue<uint32_t>*>(handle_type->rights)->value,
      3u);
}

TEST(HandleTests, GoodNoHandleRightsTest) {
  TestLibrary library(R"FIDL(
library example;

using zx;

type MyStruct = resource struct {
    h zx.Handle:VMO;
};
)FIDL");
  library.UseLibraryZx();

  ASSERT_COMPILED(library);

  const auto& h_type_ctor = library.LookupStruct("MyStruct")->members[0].type_ctor;
  auto h_type = h_type_ctor->type;
  ASSERT_NE(h_type, nullptr);
  ASSERT_EQ(h_type->kind, fidl::flat::Type::Kind::kHandle);
  auto handle_type = static_cast<const fidl::flat::HandleType*>(h_type);

  ASSERT_TRUE(h_type_ctor->resolved_params.subtype_raw != nullptr);
  EXPECT_EQ("VMO", h_type_ctor->resolved_params.subtype_raw->span.data());
  EXPECT_EQ(fidl::types::HandleSubtype::kVmo, handle_type->subtype);
  EXPECT_EQ(
      static_cast<const fidl::flat::NumericConstantValue<uint32_t>*>(handle_type->rights)->value,
      fidl::flat::kHandleSameRights);
}

TEST(HandleTests, BadInvalidHandleRightsTest) {
  TestLibrary library(R"FIDL(
library example;

using zx;

protocol P {
    Method(struct { h zx.Handle:<VMO, 1>; });  // rights must be zx.Rights-typed.
};
)FIDL");
  library.UseLibraryZx();

  // NOTE(https://fxbug.dev/72924): we provide a more general error because there are multiple
  // possible interpretations.
  library.ExpectFail(fidl::ErrTypeCannotBeConvertedToType, "1", "untyped numeric", "zx/Rights");
  library.ExpectFail(fidl::ErrUnexpectedConstraint, "Handle");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(HandleTests, GoodPlainHandleTest) {
  TestLibrary library(R"FIDL(
library example;

using zx;

type MyStruct = resource struct {
    h zx.Handle;
};
)FIDL");
  library.UseLibraryZx();
  ASSERT_COMPILED(library);

  const auto& h_type_ctor = library.LookupStruct("MyStruct")->members[0].type_ctor;

  auto h_type = h_type_ctor->type;
  ASSERT_NE(h_type, nullptr);
  ASSERT_EQ(h_type->kind, fidl::flat::Type::Kind::kHandle);
  auto handle_type = static_cast<const fidl::flat::HandleType*>(h_type);

  EXPECT_EQ(fidl::types::HandleSubtype::kHandle, handle_type->subtype);
  EXPECT_EQ(
      static_cast<const fidl::flat::NumericConstantValue<uint32_t>*>(handle_type->rights)->value,
      fidl::flat::kHandleSameRights);
}

TEST(HandleTests, GoodHandleFidlDefinedTest) {
  TestLibrary library(R"FIDL(
library example;

using zx;

type MyStruct = resource struct {
  a zx.Handle:THREAD;
  b zx.Handle:<PROCESS>;
  c zx.Handle:<VMO, zx.Rights.TRANSFER>;
};
)FIDL");
  library.UseLibraryZx();

  ASSERT_COMPILED(library);
  const auto& a = library.LookupStruct("MyStruct")->members[0].type_ctor;
  auto a_type = a->type;
  ASSERT_NE(a_type, nullptr);
  ASSERT_EQ(a_type->kind, fidl::flat::Type::Kind::kHandle);
  auto a_handle_type = static_cast<const fidl::flat::HandleType*>(a_type);
  EXPECT_EQ(fidl::types::HandleSubtype::kThread, a_handle_type->subtype);
  EXPECT_EQ(static_cast<const fidl::flat::HandleRights*>(a_handle_type->rights)->value,
            fidl::flat::kHandleSameRights);

  const auto& b = library.LookupStruct("MyStruct")->members[1].type_ctor;
  auto b_type = b->type;
  ASSERT_NE(b_type, nullptr);
  ASSERT_EQ(b_type->kind, fidl::flat::Type::Kind::kHandle);
  auto b_handle_type = static_cast<const fidl::flat::HandleType*>(b_type);
  EXPECT_EQ(fidl::types::HandleSubtype::kProcess, b_handle_type->subtype);
  EXPECT_EQ(static_cast<const fidl::flat::HandleRights*>(b_handle_type->rights)->value,
            fidl::flat::kHandleSameRights);

  const auto& c = library.LookupStruct("MyStruct")->members[2].type_ctor;
  auto c_type = c->type;
  ASSERT_NE(c_type, nullptr);
  ASSERT_EQ(c_type->kind, fidl::flat::Type::Kind::kHandle);
  auto c_handle_type = static_cast<const fidl::flat::HandleType*>(c_type);
  EXPECT_EQ(fidl::types::HandleSubtype::kVmo, c_handle_type->subtype);
  ASSERT_NE(c_handle_type->rights, nullptr);
  EXPECT_EQ(static_cast<const fidl::flat::HandleRights*>(c_handle_type->rights)->value, 2u);
}

TEST(HandleTests, BadInvalidFidlDefinedHandleSubtype) {
  TestLibrary library(R"FIDL(
library example;

using zx;

type MyStruct = struct {
  a zx.Handle:ZIPPY;
};
)FIDL");
  library.UseLibraryZx();

  library.ExpectFail(fidl::ErrNameNotFound, "ZIPPY", "example");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(HandleTests, BadDisallowOldHandles) {
  TestLibrary library(R"FIDL(
library example;

using zx;

type MyStruct = struct {
    h handle<vmo>;
};
)FIDL");
  library.UseLibraryZx();

  library.ExpectFail(fidl::ErrNameNotFound, "handle", "example");
  library.ExpectFail(fidl::ErrNameNotFound, "vmo", "example");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(HandleTests, GoodResourceDefinitionOnlySubtypeNoRightsTest) {
  TestLibrary library(R"FIDL(library example;

type ObjType = strict enum : uint32 {
    NONE = 0;
    VMO = 3;
};

resource_definition handle : uint32 {
    properties {
        subtype ObjType;
    };
};

type MyStruct = resource struct {
    h handle:VMO;
};
)FIDL");

  ASSERT_COMPILED(library);

  const auto& h_type_ctor = library.LookupStruct("MyStruct")->members[0].type_ctor;
  auto h_type = h_type_ctor->type;
  ASSERT_NE(h_type, nullptr);
  ASSERT_EQ(h_type->kind, fidl::flat::Type::Kind::kHandle);
  auto handle_type = static_cast<const fidl::flat::HandleType*>(h_type);

  ASSERT_TRUE(h_type_ctor->resolved_params.subtype_raw != nullptr);
  EXPECT_TRUE(h_type_ctor->resolved_params.subtype_raw->span.data() == "VMO");
  EXPECT_EQ(fidl::types::HandleSubtype::kVmo, handle_type->subtype);
  EXPECT_EQ(
      static_cast<const fidl::flat::NumericConstantValue<uint32_t>*>(handle_type->rights)->value,
      fidl::flat::kHandleSameRights);
}

TEST(HandleTests, BadInvalidSubtypeAtUseSite) {
  TestLibrary library(R"FIDL(
library example;

type ObjType = enum : uint32 {
    NONE = 0;
    VMO = 3;
};

resource_definition handle : uint32 {
    properties {
        subtype ObjType;
    };
};

type MyStruct = resource struct {
    h handle:<1, optional>;
};
)FIDL");

  library.ExpectFail(fidl::ErrTypeCannotBeConvertedToType, "1", "untyped numeric",
                     "example/ObjType");
  library.ExpectFail(fidl::ErrUnexpectedConstraint, "handle");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(HandleTests, BadInvalidRightsAtUseSite) {
  TestLibrary library(R"FIDL(
library example;

type ObjType = enum : uint32 {
    NONE = 0;
    VMO = 3;
};

resource_definition handle : uint32 {
    properties {
        subtype ObjType;
        rights uint32;
    };
};

type MyStruct = resource struct {
    h handle:<VMO, "my_improperly_typed_rights", optional>;
};
)FIDL");

  library.ExpectFail(fidl::ErrTypeCannotBeConvertedToType, "\"my_improperly_typed_rights\"",
                     "string:26", "uint32");
  library.ExpectFail(fidl::ErrUnexpectedConstraint, "handle");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(HandleTests, BadBareHandleNoConstraints) {
  TestLibrary library(R"FIDL(
library example;

type MyStruct = resource struct {
    h handle;
};
)FIDL");
  library.ExpectFail(fidl::ErrNameNotFound, "handle", "example");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(HandleTests, BadBareHandleWithConstraints) {
  TestLibrary library(R"FIDL(
library example;

type MyStruct = resource struct {
    h handle:VMO;
};
)FIDL");
  library.ExpectFail(fidl::ErrNameNotFound, "handle", "example");
  library.ExpectFail(fidl::ErrNameNotFound, "VMO", "example");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(HandleTests, BadBareHandleWithConstraintsThroughAlias) {
  TestLibrary library(R"FIDL(
library example;

alias my_handle = handle;

type MyStruct = resource struct {
    h my_handle:VMO;
};
)FIDL");
  library.ExpectFail(fidl::ErrNameNotFound, "handle", "example");
  library.ExpectFail(fidl::ErrNameNotFound, "VMO", "example");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

}  // namespace
