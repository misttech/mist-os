// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zircon/types.h>

#include <string>
#include <tuple>

#include <zxtest/zxtest.h>

#include "tools/fidl/fidlc/include/fidl/token.h"
#include "tools/fidl/fidlc/include/fidl/token_list.h"
#include "tools/fidl/fidlc/tests/error_test.h"
#include "tools/fidl/fidlc/tests/test_library.h"

namespace {

// For each element, check if the pointer in the TokenList is into a |Token| held by a
// |SourceElement| start/end field included in the raw AST, or if it has been "discarded" and is
// only stored in the |tokens| side list.
enum struct Storage {
  kAST,
  kSide,
};

using Kind = fidl::Token::Kind;
using WantToken = std::tuple<std::string, Kind, Storage>;
using Expected = std::vector<WantToken>;

// Compare the |TokenPointerList| to our expectations. Uses |ASSERT_*| test helpers for easier
// debugging.
void Compare(const std::unique_ptr<fidl::raw::File>& ast, fidl::raw::TokenPointerList actual,
             Expected expected) {
  const void* side_storage_start = &(ast->tokens.front());
  const void* side_storage_end = &(ast->tokens.back());
  ASSERT_EQ(expected.size(), actual.size());
  for (size_t i = 0; i < actual.size(); i++) {
    const auto act = actual[i];
    const auto exp = expected[i];
    if (act->kind() != Kind::kStartOfFile && act->kind() != Kind::kEndOfFile) {
      ASSERT_STREQ(act->span().data(), std::get<std::string>(exp));
    }

    ASSERT_EQ(act->kind(), std::get<Kind>(exp));
    if (side_storage_start <= act && side_storage_end >= act) {
      // |act| pointer is pointing into the side list.
      ASSERT_EQ(std::get<Storage>(exp), Storage::kSide);
    } else {
      // |act| pointer is pointing into the raw AST.
      ASSERT_EQ(std::get<Storage>(exp), Storage::kAST);
    }
  }
}

TEST(TokenListTests, MinimalSideTokens) {
  TestLibrary library(R"FIDL(library example;
)FIDL");
  std::unique_ptr<fidl::raw::File> ast;
  ASSERT_TRUE(library.Parse(&ast));

  Compare(ast, fidl::raw::TokenPointerListBuilder(ast).Build(),
          {
              {"", Kind::kStartOfFile, Storage::kAST},
              {"library", Kind::kIdentifier, Storage::kAST},
              {"example", Kind::kIdentifier, Storage::kAST},
              {";", Kind::kSemicolon, Storage::kSide},
              {"", Kind::kEndOfFile, Storage::kAST},
          });
}

TEST(TokenListTests, MoreSideTokens) {
  TestLibrary library(R"FIDL(library example.lib;
)FIDL");
  std::unique_ptr<fidl::raw::File> ast;
  ASSERT_TRUE(library.Parse(&ast));

  Compare(ast, fidl::raw::TokenPointerListBuilder(ast).Build(),
          {
              {"", Kind::kStartOfFile, Storage::kAST},
              {"library", Kind::kIdentifier, Storage::kAST},
              {"example", Kind::kIdentifier, Storage::kAST},
              {".", Kind::kDot, Storage::kSide},
              {"lib", Kind::kIdentifier, Storage::kAST},
              {";", Kind::kSemicolon, Storage::kSide},
              {"", Kind::kEndOfFile, Storage::kAST},
          });
}

TEST(TokenListTests, WithAttribute) {
  TestLibrary library(R"FIDL(
@attr(foo = "bar")
library example;
)FIDL");
  std::unique_ptr<fidl::raw::File> ast;
  ASSERT_TRUE(library.Parse(&ast));

  Compare(ast, fidl::raw::TokenPointerListBuilder(ast).Build(),
          {
              {"", Kind::kStartOfFile, Storage::kAST},
              {"@", Kind::kAt, Storage::kAST},
              {"attr", Kind::kIdentifier, Storage::kSide},
              {"(", Kind::kLeftParen, Storage::kSide},
              {"foo", Kind::kIdentifier, Storage::kAST},
              {"=", Kind::kEqual, Storage::kSide},
              {"\"bar\"", Kind::kStringLiteral, Storage::kAST},
              // This paren is the end |Token| for the |raw::Attribute|, so it is in the AST.
              {")", Kind::kRightParen, Storage::kAST},
              // Note that because this "library" no longer the first token in the |raw::File| span,
              // it is now in the side list. The "@" symbol takes its spot in the AST.
              {"library", Kind::kIdentifier, Storage::kSide},
              {"example", Kind::kIdentifier, Storage::kAST},
              {";", Kind::kSemicolon, Storage::kSide},
              {"", Kind::kEndOfFile, Storage::kAST},
          });
}

TEST(TokenListTests, WithComments) {
  TestLibrary library(R"FIDL(
// A
// B
library example;
)FIDL");
  std::unique_ptr<fidl::raw::File> ast;
  ASSERT_TRUE(library.Parse(&ast));

  Compare(ast, fidl::raw::TokenPointerListBuilder(ast).Build(),
          {
              {"", Kind::kStartOfFile, Storage::kAST},
              {"// A", Kind::kComment, Storage::kSide},
              {"// B", Kind::kComment, Storage::kSide},
              {"library", Kind::kIdentifier, Storage::kAST},
              {"example", Kind::kIdentifier, Storage::kAST},
              {";", Kind::kSemicolon, Storage::kSide},
              {"", Kind::kEndOfFile, Storage::kAST},
          });
}

TEST(TokenListTests, WithDeclaration) {
  TestLibrary library(R"FIDL(library example;

alias Foo = vector<uint8>;
)FIDL");
  std::unique_ptr<fidl::raw::File> ast;
  ASSERT_TRUE(library.Parse(&ast));

  Compare(ast, fidl::raw::TokenPointerListBuilder(ast).Build(),
          {
              {"", Kind::kStartOfFile, Storage::kAST},
              {"library", Kind::kIdentifier, Storage::kAST},
              {"example", Kind::kIdentifier, Storage::kAST},
              {";", Kind::kSemicolon, Storage::kSide},
              {"alias", Kind::kIdentifier, Storage::kAST},
              {"Foo", Kind::kIdentifier, Storage::kAST},
              {"=", Kind::kEqual, Storage::kSide},
              {"vector", Kind::kIdentifier, Storage::kAST},
              {"<", Kind::kLeftAngle, Storage::kAST},
              {"uint8", Kind::kIdentifier, Storage::kAST},
              {">", Kind::kRightAngle, Storage::kAST},
              {";", Kind::kSemicolon, Storage::kSide},
              {"", Kind::kEndOfFile, Storage::kAST},
          });
}

}  // namespace
