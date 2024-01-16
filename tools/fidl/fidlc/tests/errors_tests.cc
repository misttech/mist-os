// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

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
  ASSERT_EQ(methods->size(), 1u);
  auto method = &methods->at(0);
  auto response = method->maybe_response.get();
  ASSERT_NE(response, nullptr);

  auto id = static_cast<const fidlc::IdentifierType*>(response->type);
  auto result_union = static_cast<const fidlc::Union*>(id->type_decl);
  ASSERT_NE(result_union, nullptr);
  ASSERT_EQ(result_union->members.size(), 2u);

  auto anonymous = result_union->name.as_anonymous();
  ASSERT_NE(anonymous, nullptr);
  ASSERT_EQ(anonymous->provenance, fidlc::Name::Provenance::kGeneratedResultUnion);

  const auto& success = result_union->members.at(0);
  ASSERT_NE(success.maybe_used, nullptr);
  ASSERT_EQ("response", success.maybe_used->name.data());

  const fidlc::Union::Member& error = result_union->members.at(1);
  ASSERT_NE(error.maybe_used, nullptr);
  ASSERT_EQ("err", error.maybe_used->name.data());

  ASSERT_NE(error.maybe_used->type_ctor->type, nullptr);
  ASSERT_EQ(error.maybe_used->type_ctor->type->kind, fidlc::Type::Kind::kPrimitive);
  auto primitive_type = static_cast<const fidlc::PrimitiveType*>(error.maybe_used->type_ctor->type);
  ASSERT_EQ(primitive_type->subtype, fidlc::PrimitiveSubtype::kInt32);
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
  ASSERT_NE(protocol, nullptr);
  ASSERT_EQ(protocol->methods.size(), 1u);

  auto& method = protocol->methods[0];
  EXPECT_TRUE(method.has_request);
  EXPECT_EQ(method.maybe_request.get(), nullptr);
  ASSERT_TRUE(method.has_response && method.maybe_response.get());

  auto id = static_cast<const fidlc::IdentifierType*>(method.maybe_response->type);
  auto response = static_cast<const fidlc::Union*>(id->type_decl);
  EXPECT_TRUE(response->kind == fidlc::Decl::Kind::kUnion);
  ASSERT_EQ(response->members.size(), 2u);

  auto empty_struct_name = response->members[0].maybe_used->type_ctor->type->name.decl_name();
  auto empty_struct = library.LookupStruct(empty_struct_name);
  ASSERT_NE(empty_struct, nullptr);
  auto anonymous = empty_struct->name.as_anonymous();
  ASSERT_EQ(anonymous->provenance, fidlc::Name::Provenance::kGeneratedEmptySuccessStruct);
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

  library.ExpectFail(fidlc::ErrNameNotFound, "ParsingError", "test.bad.fi0052");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ErrorsTests, BadErrorWrongPrimitive) {
  TestLibrary library;
  library.AddFile("bad/fi-0141.test.fidl");

  library.ExpectFail(fidlc::ErrInvalidErrorType);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ErrorsTests, BadErrorMissingType) {
  TestLibrary library(R"FIDL(
library example;
protocol Example {
    Method() -> (flub int32) error;
};
)FIDL");
  library.ExpectFail(fidlc::ErrUnexpectedTokenOfKind,
                     fidlc::Token::KindAndSubkind(fidlc::Token::Kind::kIdentifier),
                     fidlc::Token::KindAndSubkind(fidlc::Token::Kind::kRightParen));
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ErrorsTests, BadErrorNotAType) {
  TestLibrary library(R"FIDL(
library example;
protocol Example {
    Method() -> (flub int32) error "hello";
};
)FIDL");
  library.ExpectFail(fidlc::ErrUnexpectedTokenOfKind,
                     fidlc::Token::KindAndSubkind(fidlc::Token::Kind::kIdentifier),
                     fidlc::Token::KindAndSubkind(fidlc::Token::Kind::kRightParen));
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ErrorsTests, BadErrorNoResponse) {
  TestLibrary library(R"FIDL(
library example;
protocol Example {
    Method() -> error int32;
};
)FIDL");
  library.ExpectFail(fidlc::ErrUnexpectedTokenOfKind,
                     fidlc::Token::KindAndSubkind(fidlc::Token::Subkind::kError),
                     fidlc::Token::KindAndSubkind(fidlc::Token::Kind::kLeftParen));
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ErrorsTests, BadErrorUnexpectedEndOfFile) {
  TestLibrary library(R"FIDL(
library example;
type ForgotTheSemicolon = table {}
)FIDL");

  library.ExpectFail(fidlc::ErrUnexpectedTokenOfKind,
                     fidlc::Token::KindAndSubkind(fidlc::Token::Kind::kEndOfFile),
                     fidlc::Token::KindAndSubkind(fidlc::Token::Kind::kSemicolon));
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ErrorsTests, BadIncorrectIdentifier) {
  TestLibrary library;
  library.AddFile("bad/fi-0009.test.fidl");
  library.ExpectFail(fidlc::ErrUnexpectedIdentifier,
                     fidlc::Token::KindAndSubkind(fidlc::Token::Subkind::kUsing),
                     fidlc::Token::KindAndSubkind(fidlc::Token::Subkind::kLibrary));
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ErrorsTests, BadErrorEmptyFile) {
  TestLibrary library("");

  library.ExpectFail(fidlc::ErrUnexpectedIdentifier,
                     fidlc::Token::KindAndSubkind(fidlc::Token::Kind::kEndOfFile),
                     fidlc::Token::KindAndSubkind(fidlc::Token::Subkind::kLibrary));
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ErrorsTests, ExperimentalAllowArbitraryErrorTypes) {
  TestLibrary library(R"FIDL(
library example;
protocol Example {
    Method() -> () error table {};
};
)FIDL");
  library.EnableFlag(fidlc::ExperimentalFlags::Flag::kAllowArbitraryErrorTypes);
  ASSERT_COMPILED(library);

  auto result_id = static_cast<const fidlc::IdentifierType*>(
      library.LookupProtocol("Example")->methods.at(0).maybe_response->type);
  auto result_union = static_cast<const fidlc::Union*>(result_id->type_decl);
  auto error_id = static_cast<const fidlc::IdentifierType*>(
      result_union->members.at(1).maybe_used->type_ctor->type);
  ASSERT_EQ(error_id->type_decl->kind, fidlc::Decl::Kind::kTable);
}

TEST(ErrorsTest, TransitionalAllowList) {
  TestLibrary library;
  library.AddFile("bad/fi-0202.test.fidl");
  library.EnableFlag(fidlc::ExperimentalFlags::Flag::kTransitionalAllowList);
  library.ExpectFail(fidlc::ErrTransitionalNotAllowed, "NewMethod");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

}  // namespace
