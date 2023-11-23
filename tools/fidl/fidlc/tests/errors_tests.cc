// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zxtest/zxtest.h>

#include "tools/fidl/fidlc/include/fidl/flat/types.h"
#include "tools/fidl/fidlc/tests/test_library.h"

namespace {

TEST(ErrorsTests, GoodError) {
  TestLibrary library(R"FIDL(library example;

protocol Example {
    strict Method() -> (struct {
        foo string;
    }) error int32;
};
)FIDL");
  ASSERT_COMPILED(library);

  auto methods = &library.LookupProtocol("Example")->methods;
  ASSERT_EQ(methods->size(), 1);
  auto method = &methods->at(0);
  auto response = method->maybe_response.get();
  ASSERT_NOT_NULL(response);

  auto id = static_cast<const fidl::flat::IdentifierType*>(response->type);
  auto result_union = static_cast<const fidl::flat::Union*>(id->type_decl);
  ASSERT_NOT_NULL(result_union);
  ASSERT_EQ(result_union->members.size(), 2);

  auto anonymous = result_union->name.as_anonymous();
  ASSERT_NOT_NULL(anonymous);
  ASSERT_EQ(anonymous->provenance, fidl::flat::Name::Provenance::kGeneratedResultUnion);

  const auto& success = result_union->members.at(0);
  ASSERT_NOT_NULL(success.maybe_used);
  ASSERT_STREQ("response", std::string(success.maybe_used->name.data()).c_str());

  const fidl::flat::Union::Member& error = result_union->members.at(1);
  ASSERT_NOT_NULL(error.maybe_used);
  ASSERT_STREQ("err", std::string(error.maybe_used->name.data()).c_str());

  ASSERT_NOT_NULL(error.maybe_used->type_ctor->type);
  ASSERT_EQ(error.maybe_used->type_ctor->type->kind, fidl::flat::Type::Kind::kPrimitive);
  auto primitive_type =
      static_cast<const fidl::flat::PrimitiveType*>(error.maybe_used->type_ctor->type);
  ASSERT_EQ(primitive_type->subtype, fidl::types::PrimitiveSubtype::kInt32);
}

TEST(ErrorsTests, GoodErrorUnsigned) {
  TestLibrary library(R"FIDL(library example;

protocol Example {
    Method() -> (struct {
        foo string;
    }) error uint32;
};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(ErrorsTests, GoodErrorEmptyStructAsSuccess) {
  TestLibrary library(R"FIDL(
library example;

protocol MyProtocol {
  strict MyMethod() -> () error uint32;
};
)FIDL");
  ASSERT_COMPILED(library);
  auto protocol = library.LookupProtocol("MyProtocol");
  ASSERT_NOT_NULL(protocol);
  ASSERT_EQ(protocol->methods.size(), 1);

  auto& method = protocol->methods[0];
  EXPECT_TRUE(method.has_request);
  EXPECT_NULL(method.maybe_request.get());
  ASSERT_TRUE(method.has_response && method.maybe_response.get());

  auto id = static_cast<const fidl::flat::IdentifierType*>(method.maybe_response->type);
  auto response = static_cast<const fidl::flat::Union*>(id->type_decl);
  EXPECT_TRUE(response->kind == fidl::flat::Decl::Kind::kUnion);
  ASSERT_EQ(response->members.size(), 2);

  auto empty_struct_name = response->members[0].maybe_used->type_ctor->type->name.decl_name();
  auto empty_struct = library.LookupStruct(empty_struct_name);
  ASSERT_NOT_NULL(empty_struct);
  auto anonymous = empty_struct->name.as_anonymous();
  ASSERT_EQ(anonymous->provenance, fidl::flat::Name::Provenance::kGeneratedEmptySuccessStruct);
}

TEST(ErrorsTests, GoodErrorEnum) {
  TestLibrary library(R"FIDL(library example;

type ErrorType = enum : int32 {
    GOOD = 1;
    BAD = 2;
    UGLY = 3;
};

protocol Example {
    Method() -> (struct {
        foo string;
    }) error ErrorType;
};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(ErrorsTests, GoodErrorEnumAfter) {
  TestLibrary library(R"FIDL(library example;

protocol Example {
    Method() -> (struct {
        foo string;
    }) error ErrorType;
};

type ErrorType = enum : int32 {
    GOOD = 1;
    BAD = 2;
    UGLY = 3;
};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(ErrorsTests, BadErrorUnknownIdentifier) {
  TestLibrary library;
  library.AddFile("bad/fi-0052.test.fidl");

  library.ExpectFail(fidl::ErrNameNotFound, "ParsingError", "test.bad.fi0052");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ErrorsTests, BadErrorWrongPrimitive) {
  TestLibrary library;
  library.AddFile("bad/fi-0141.test.fidl");

  library.ExpectFail(fidl::ErrInvalidErrorType);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ErrorsTests, BadErrorMissingType) {
  TestLibrary library(R"FIDL(
library example;
protocol Example {
    Method() -> (flub int32) error;
};
)FIDL");
  library.ExpectFail(fidl::ErrUnexpectedTokenOfKind,
                     fidl::Token::KindAndSubkind(fidl::Token::Kind::kIdentifier),
                     fidl::Token::KindAndSubkind(fidl::Token::Kind::kRightParen));
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ErrorsTests, BadErrorNotAType) {
  TestLibrary library(R"FIDL(
library example;
protocol Example {
    Method() -> (flub int32) error "hello";
};
)FIDL");
  library.ExpectFail(fidl::ErrUnexpectedTokenOfKind,
                     fidl::Token::KindAndSubkind(fidl::Token::Kind::kIdentifier),
                     fidl::Token::KindAndSubkind(fidl::Token::Kind::kRightParen));
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ErrorsTests, BadErrorNoResponse) {
  TestLibrary library(R"FIDL(
library example;
protocol Example {
    Method() -> error int32;
};
)FIDL");
  library.ExpectFail(fidl::ErrUnexpectedTokenOfKind,
                     fidl::Token::KindAndSubkind(fidl::Token::Subkind::kError),
                     fidl::Token::KindAndSubkind(fidl::Token::Kind::kLeftParen));
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ErrorsTests, BadErrorUnexpectedEndOfFile) {
  TestLibrary library(R"FIDL(
library example;
type ForgotTheSemicolon = table {}
)FIDL");

  library.ExpectFail(fidl::ErrUnexpectedTokenOfKind,
                     fidl::Token::KindAndSubkind(fidl::Token::Kind::kEndOfFile),
                     fidl::Token::KindAndSubkind(fidl::Token::Kind::kSemicolon));
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ErrorsTests, BadIncorrectIdentifier) {
  TestLibrary library;
  library.AddFile("bad/fi-0009.test.fidl");
  library.ExpectFail(fidl::ErrUnexpectedIdentifier,
                     fidl::Token::KindAndSubkind(fidl::Token::Subkind::kUsing),
                     fidl::Token::KindAndSubkind(fidl::Token::Subkind::kLibrary));
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ErrorsTests, BadErrorEmptyFile) {
  TestLibrary library("");

  library.ExpectFail(fidl::ErrUnexpectedIdentifier,
                     fidl::Token::KindAndSubkind(fidl::Token::Kind::kEndOfFile),
                     fidl::Token::KindAndSubkind(fidl::Token::Subkind::kLibrary));
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ErrorsTests, ExperimentalAllowArbitraryErrorTypes) {
  TestLibrary library(R"FIDL(
library example;
protocol Example {
    Method() -> () error table {};
};
)FIDL");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kAllowArbitraryErrorTypes);
  ASSERT_COMPILED(library);

  auto result_id = static_cast<const fidl::flat::IdentifierType*>(
      library.LookupProtocol("Example")->methods.at(0).maybe_response->type);
  auto result_union = static_cast<const fidl::flat::Union*>(result_id->type_decl);
  auto error_id = static_cast<const fidl::flat::IdentifierType*>(
      result_union->members.at(1).maybe_used->type_ctor->type);
  ASSERT_EQ(error_id->type_decl->kind, fidl::flat::Decl::Kind::kTable);
}

TEST(ErrorsTest, TransitionalAllowList) {
  TestLibrary library;
  library.AddFile("bad/fi-0202.test.fidl");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kTransitionalAllowList);
  library.ExpectFail(fidl::ErrTransitionalNotAllowed, "NewMethod");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

}  // namespace
