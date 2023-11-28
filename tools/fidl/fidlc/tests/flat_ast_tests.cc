// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zxtest/zxtest.h>

#include "tools/fidl/fidlc/include/fidl/diagnostics.h"
#include "tools/fidl/fidlc/include/fidl/flat_ast.h"
#include "tools/fidl/fidlc/include/fidl/source_file.h"
#include "tools/fidl/fidlc/include/fidl/source_span.h"
#include "tools/fidl/fidlc/tests/test_library.h"

namespace {

using fidl::SourceFile;
using fidl::SourceSpan;
using fidl::Token;
using fidl::flat::HandleRights;
using fidl::flat::HandleType;
using fidl::flat::LiteralConstant;
using fidl::flat::Name;
using fidl::raw::Literal;
using fidl::raw::SourceElement;
using fidl::types::HandleSubtype;
using fidl::types::Nullability;
using fidl::types::RightsWrappedType;

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
  auto fake_literal = Literal(fake_source_element, Literal::Kind::kNumeric);
  auto rights1Constant = std::make_unique<LiteralConstant>(&fake_literal);
  rights1Constant->ResolveTo(std::make_unique<HandleRights>(1), nullptr);
  auto rights1Value = static_cast<const HandleRights*>(&rights1Constant->Value());
  auto rights2Constant = std::make_unique<LiteralConstant>(&fake_literal);
  rights2Constant->ResolveTo(std::make_unique<HandleRights>(2), nullptr);
  auto rights2Value = static_cast<const HandleRights*>(&rights2Constant->Value());
  fidl::flat::Resource* resource_decl_not_needed = nullptr;
  HandleType nonnullable_channel_rights1(
      name_not_important, resource_decl_not_needed,
      HandleType::Constraints(fidl::types::HandleSubtype::kChannel, rights1Value,
                              Nullability::kNonnullable));
  HandleType nullable_channel_rights1(
      name_not_important, resource_decl_not_needed,
      HandleType::Constraints(fidl::types::HandleSubtype::kChannel, rights1Value,
                              Nullability::kNullable));
  HandleType nonnullable_event_rights1(
      name_not_important, resource_decl_not_needed,
      HandleType::Constraints(fidl::types::HandleSubtype::kEvent, rights1Value,
                              Nullability::kNonnullable));
  HandleType nullable_event_rights1(name_not_important, resource_decl_not_needed,
                                    HandleType::Constraints(fidl::types::HandleSubtype::kEvent,
                                                            rights1Value, Nullability::kNullable));
  HandleType nullable_event_rights2(name_not_important, resource_decl_not_needed,
                                    HandleType::Constraints(fidl::types::HandleSubtype::kEvent,
                                                            rights2Value, Nullability::kNullable));

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

  library.ExpectFail(fidl::ErrAnonymousNameReference, "MyProtocolMyInfallibleRequest");
  library.ExpectFail(fidl::ErrAnonymousNameReference, "MyProtocolMyInfallibleResponse");
  library.ExpectFail(fidl::ErrAnonymousNameReference, "MyProtocolMyFallibleRequest");
  library.ExpectFail(fidl::ErrAnonymousNameReference, "MyProtocol_MyFallible_Result");
  library.ExpectFail(fidl::ErrAnonymousNameReference, "MyProtocol_MyFallible_Response");
  library.ExpectFail(fidl::ErrAnonymousNameReference, "MyProtocol_MyFallible_Error");
  library.ExpectFail(fidl::ErrAnonymousNameReference, "MyProtocolMyEventRequest");

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
  library.ExpectFail(fidl::ErrNameCollision, fidl::flat::Element::Kind::kStruct,
                     "FooSomeMethodRequest", fidl::flat::Element::Kind::kStruct,
                     "example.fidl:5:14");
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
  library2.ExpectFail(fidl::ErrMultipleLibrariesWithSameName, "test.bad.fi0041");
  ASSERT_COMPILER_DIAGNOSTICS(library2);
}

}  // namespace
