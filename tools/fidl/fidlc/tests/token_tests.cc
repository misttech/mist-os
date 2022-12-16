// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zircon/types.h>

#include <zxtest/zxtest.h>

#include "tools/fidl/fidlc/include/fidl/source_file.h"
#include "tools/fidl/fidlc/include/fidl/token.h"

namespace {

using Kind = fidl::Token::Kind;
using Subkind = fidl::Token::Subkind;

TEST(TokenTests, SameToken) {
  auto file = fidl::SourceFile("a", "a");
  auto start = fidl::SourceSpan(file.data().substr(0, 0), file);
  auto letter = fidl::SourceSpan(file.data().substr(0, 1), file);
  auto token = fidl::Token(start, letter, 0, Kind::kIdentifier, Subkind::kNone, 0);
  auto copy = fidl::Token(token);

  EXPECT_TRUE(token.same_file_as(fidl::Token(copy)));
  EXPECT_TRUE(token == copy);
  EXPECT_FALSE(token != copy);
  EXPECT_TRUE(token <= copy);
  EXPECT_TRUE(token >= copy);
  EXPECT_FALSE(token < copy);
  EXPECT_FALSE(token > copy);

  // Reverse operand order.
  EXPECT_TRUE(copy == token);
  EXPECT_FALSE(copy != token);
  EXPECT_TRUE(copy <= token);
  EXPECT_TRUE(copy >= token);
  EXPECT_FALSE(copy < token);
  EXPECT_FALSE(copy > token);
}

TEST(TokenTests, SameFileDifferentToken) {
  auto file = fidl::SourceFile("a", "a a");
  auto start = fidl::SourceSpan(file.data().substr(0, 0), file);
  auto first = fidl::SourceSpan(file.data().substr(0, 1), file);
  auto left = fidl::Token(start, first, 0, Kind::kIdentifier, Subkind::kNone, 0);
  auto space = fidl::SourceSpan(file.data().substr(1, 1), file);
  auto second = fidl::SourceSpan(file.data().substr(2, 1), file);
  auto right = fidl::Token(space, second, 0, Kind::kIdentifier, Subkind::kNone, 1);

  EXPECT_TRUE(left.same_file_as(right));
  EXPECT_FALSE(left == right);
  EXPECT_TRUE(left != right);
  EXPECT_TRUE(left <= right);
  EXPECT_FALSE(left >= right);
  EXPECT_TRUE(left < right);
  EXPECT_FALSE(left > right);

  // Reverse operand order.
  EXPECT_TRUE(right.same_file_as(left));
  EXPECT_FALSE(right == left);
  EXPECT_TRUE(right != left);
  EXPECT_FALSE(right <= left);
  EXPECT_TRUE(right >= left);
  EXPECT_FALSE(right < left);
  EXPECT_TRUE(right > left);
}

TEST(TokenTests, DifferentFileDifferentToken) {
  auto file_a = fidl::SourceFile("a", "a");
  auto start_a = fidl::SourceSpan(file_a.data().substr(0, 0), file_a);
  auto letter_a = fidl::SourceSpan(file_a.data().substr(0, 1), file_a);
  auto token_a = fidl::Token(start_a, letter_a, 0, Kind::kIdentifier, Subkind::kNone, 0);

  auto file_b = fidl::SourceFile("b", "b");
  auto start_b = fidl::SourceSpan(file_b.data().substr(0, 0), file_b);
  auto letter_b = fidl::SourceSpan(file_b.data().substr(0, 1), file_b);
  auto token_b = fidl::Token(start_b, letter_b, 0, Kind::kIdentifier, Subkind::kNone, 0);

  EXPECT_FALSE(token_a.same_file_as(token_b));
  EXPECT_FALSE(token_a == token_b);
  EXPECT_TRUE(token_a != token_b);
  EXPECT_TRUE(token_a <= token_b);
  EXPECT_FALSE(token_a >= token_b);
  EXPECT_TRUE(token_a < token_b);
  EXPECT_FALSE(token_a > token_b);

  // Reverse operand order.
  EXPECT_FALSE(token_b.same_file_as(token_a));
  EXPECT_FALSE(token_b == token_a);
  EXPECT_TRUE(token_b != token_a);
  EXPECT_FALSE(token_b <= token_a);
  EXPECT_TRUE(token_b >= token_a);
  EXPECT_FALSE(token_b < token_a);
  EXPECT_TRUE(token_b > token_a);
}

}  // namespace
