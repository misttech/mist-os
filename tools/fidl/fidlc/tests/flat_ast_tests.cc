// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "tools/fidl/fidlc/src/diagnostics.h"
#include "tools/fidl/fidlc/src/flat_ast.h"
#include "tools/fidl/fidlc/src/source_file.h"
#include "tools/fidl/fidlc/src/source_span.h"
#include "tools/fidl/fidlc/tests/test_library.h"

namespace fidlc {
namespace {

TEST(FlatAstTests, GoodImplicitAssumptions) {
  // Preconditions to unit test cases: if these change, we need to rewrite the tests themselves.
  EXPECT_TRUE(HandleSubtype::kChannel < HandleSubtype::kEvent);
  EXPECT_TRUE(Nullability::kNullable < Nullability::kNonnullable);
}

TEST(FlatAstTests, GoodCompareHandles) {
  auto name_not_important = Name::CreateIntrinsic(nullptr, "ignore");
  auto fake_source_file = SourceFile("fake.fidl", "123");
  auto fake_source_span = SourceSpan(fake_source_file.data(), fake_source_file);
  auto fake_token = Token(fake_source_span, 0, Token::Kind::kNumericLiteral, Token::Subkind::kNone);
  auto fake_source_element = SourceElement(fake_token, fake_token);
  auto fake_literal = RawLiteral(fake_source_element, RawLiteral::Kind::kNumeric);
  auto rights1Constant = std::make_unique<LiteralConstant>(&fake_literal);
  rights1Constant->ResolveTo(std::make_unique<HandleRightsValue>(1), nullptr);
  auto rights1Value = static_cast<const HandleRightsValue*>(&rights1Constant->Value());
  auto rights2Constant = std::make_unique<LiteralConstant>(&fake_literal);
  rights2Constant->ResolveTo(std::make_unique<HandleRightsValue>(2), nullptr);
  auto rights2Value = static_cast<const HandleRightsValue*>(&rights2Constant->Value());
  Resource* resource_decl_not_needed = nullptr;
  HandleType nonnullable_channel_rights1(
      name_not_important, resource_decl_not_needed,
      HandleType::Constraints(HandleSubtype::kChannel, rights1Value, Nullability::kNonnullable));
  HandleType nullable_channel_rights1(
      name_not_important, resource_decl_not_needed,
      HandleType::Constraints(HandleSubtype::kChannel, rights1Value, Nullability::kNullable));
  HandleType nonnullable_event_rights1(
      name_not_important, resource_decl_not_needed,
      HandleType::Constraints(HandleSubtype::kEvent, rights1Value, Nullability::kNonnullable));
  HandleType nullable_event_rights1(
      name_not_important, resource_decl_not_needed,
      HandleType::Constraints(HandleSubtype::kEvent, rights1Value, Nullability::kNullable));
  HandleType nullable_event_rights2(
      name_not_important, resource_decl_not_needed,
      HandleType::Constraints(HandleSubtype::kEvent, rights2Value, Nullability::kNullable));

  // Comparison is nullability, then type.
  EXPECT_TRUE(nullable_channel_rights1 < nonnullable_channel_rights1);
  EXPECT_TRUE(nullable_event_rights1 < nonnullable_event_rights1);
  EXPECT_TRUE(nonnullable_channel_rights1 < nonnullable_event_rights1);
  EXPECT_TRUE(nullable_channel_rights1 < nullable_event_rights1);
  EXPECT_TRUE(nullable_event_rights1 < nullable_event_rights2);
}

TEST(FlatAstTests, BadCannotReferenceAnonymousName) {
  TestLibrary library;
  library.AddFile("bad/fi-0058.test.fidl");

  library.ExpectFail(ErrAnonymousNameReference, "MyProtocolMyInfallibleRequest");
  library.ExpectFail(ErrAnonymousNameReference, "MyProtocolMyInfallibleResponse");
  library.ExpectFail(ErrAnonymousNameReference, "MyProtocolMyFallibleRequest");
  library.ExpectFail(ErrAnonymousNameReference, "MyProtocol_MyFallible_Result");
  library.ExpectFail(ErrAnonymousNameReference, "MyProtocol_MyFallible_Response");
  library.ExpectFail(ErrAnonymousNameReference, "MyProtocol_MyFallible_Error");
  library.ExpectFail(ErrAnonymousNameReference, "MyProtocolMyEventRequest");

  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(FlatAstTests, BadAnonymousNameConflict) {
  TestLibrary library(R"FIDL(
library example;

protocol Foo {
  SomeMethod(struct { some_param uint8; });
};

type FooSomeMethodRequest = struct {};
)FIDL");
  library.ExpectFail(ErrNameCollision, Element::Kind::kStruct, "FooSomeMethodRequest",
                     Element::Kind::kStruct, "example.fidl:5:14");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(FlatAstTests, GoodSingleAnonymousNameUse) {
  TestLibrary library(R"FIDL(library example;

protocol Foo {
    SomeMethod() -> (struct {
        some_param uint8;
    }) error uint32;
};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(FlatAstTests, BadMultipleLibrariesSameName) {
  SharedAmongstLibraries shared;
  TestLibrary library1(&shared);
  library1.AddFile("bad/fi-0041-a.test.fidl");
  ASSERT_COMPILED(library1);
  TestLibrary library2(&shared);
  library2.AddFile("bad/fi-0041-b.test.fidl");
  library2.ExpectFail(ErrMultipleLibrariesWithSameName, "test.bad.fi0041");
  ASSERT_COMPILER_DIAGNOSTICS(library2);
}

}  // namespace
}  // namespace fidlc
