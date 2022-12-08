// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zircon/types.h>

#include <string>

#include <zxtest/zxtest.h>

#include "tools/fidl/fidlc/include/fidl/raw_ast.h"

namespace {

using Signature = fidl::raw::SourceElement::Signature;
using NodeKind = fidl::raw::SourceElement::NodeKind;

TEST(SourceSignatureTests, Hashing) {
  std::string source = "one two two";
  std::string_view left = std::string_view(source).substr(0, 3);
  std::string_view middle = std::string_view(source).substr(4, 3);
  std::string_view right = std::string_view(source).substr(8, 3);
  ASSERT_EQ(middle, right);

  // Same kind, same span, same text.
  EXPECT_EQ(std::hash<Signature>{}(Signature(NodeKind::kIdentifier, left)),
            std::hash<Signature>{}(Signature(NodeKind::kIdentifier, left)));

  // Same kind, different span, different text.
  EXPECT_NE(std::hash<Signature>{}(Signature(NodeKind::kIdentifier, left)),
            std::hash<Signature>{}(Signature(NodeKind::kIdentifier, right)));

  // Same kind, different span, same text.
  EXPECT_NE(std::hash<Signature>{}(Signature(NodeKind::kIdentifier, middle)),
            std::hash<Signature>{}(Signature(NodeKind::kIdentifier, right)));

  // Different kind, same span, same text.
  EXPECT_NE(std::hash<Signature>{}(Signature(NodeKind::kIdentifier, left)),
            std::hash<Signature>{}(Signature(NodeKind::kCompoundIdentifier, left)));

  // Different kind, different span, different text.
  EXPECT_NE(std::hash<Signature>{}(Signature(NodeKind::kIdentifier, left)),
            std::hash<Signature>{}(Signature(NodeKind::kCompoundIdentifier, right)));

  // Different kind, different span, same text.
  EXPECT_NE(std::hash<Signature>{}(Signature(NodeKind::kIdentifier, middle)),
            std::hash<Signature>{}(Signature(NodeKind::kCompoundIdentifier, right)));
}

TEST(SourceSignatureTests, Equality) {
  std::string source = "one two two";
  std::string_view left = std::string_view(source).substr(0, 3);
  std::string_view middle = std::string_view(source).substr(4, 3);
  std::string_view right = std::string_view(source).substr(8, 3);
  ASSERT_EQ(middle, right);

  // Same kind, same span, same text.
  EXPECT_EQ(Signature(NodeKind::kIdentifier, left), Signature(NodeKind::kIdentifier, left));

  // Same kind, different span, different text.
  EXPECT_NE(Signature(NodeKind::kIdentifier, left), Signature(NodeKind::kIdentifier, right));

  // Same kind, different span, same text.
  EXPECT_NE(Signature(NodeKind::kIdentifier, middle), Signature(NodeKind::kIdentifier, right));

  // Different kind, same span, same text.
  EXPECT_NE(Signature(NodeKind::kIdentifier, left), Signature(NodeKind::kCompoundIdentifier, left));

  // Different kind, different span, different text.
  EXPECT_NE(Signature(NodeKind::kIdentifier, left),
            Signature(NodeKind::kCompoundIdentifier, right));

  // Different kind, different span, same text.
  EXPECT_NE(Signature(NodeKind::kIdentifier, middle),
            Signature(NodeKind::kCompoundIdentifier, right));
}

TEST(SourceSignatureTests, Ordering) {
  // Use "z" to ensure lexicographic ordering is not used.
  std::string source = "z a b";
  std::string_view left = std::string_view(source).substr(0, 1);
  std::string_view middle = std::string_view(source).substr(2, 1);
  std::string_view right = std::string_view(source).substr(4, 1);

  // left vs right
  EXPECT_TRUE(Signature(NodeKind::kIdentifier, left) < Signature(NodeKind::kIdentifier, right));
  EXPECT_TRUE(Signature(NodeKind::kIdentifier, left) <
              Signature(NodeKind::kCompoundIdentifier, right));
  EXPECT_TRUE(Signature(NodeKind::kCompoundIdentifier, left) <
              Signature(NodeKind::kIdentifier, right));
  EXPECT_FALSE(Signature(NodeKind::kIdentifier, right) < Signature(NodeKind::kIdentifier, left));
  EXPECT_FALSE(Signature(NodeKind::kCompoundIdentifier, right) <
               Signature(NodeKind::kIdentifier, left));
  EXPECT_FALSE(Signature(NodeKind::kCompoundIdentifier, right) <
               Signature(NodeKind::kIdentifier, left));

  // left vs middle
  EXPECT_TRUE(Signature(NodeKind::kIdentifier, left) < Signature(NodeKind::kIdentifier, middle));
  EXPECT_TRUE(Signature(NodeKind::kIdentifier, left) <
              Signature(NodeKind::kCompoundIdentifier, middle));
  EXPECT_TRUE(Signature(NodeKind::kCompoundIdentifier, left) <
              Signature(NodeKind::kIdentifier, middle));
  EXPECT_FALSE(Signature(NodeKind::kIdentifier, middle) < Signature(NodeKind::kIdentifier, left));
  EXPECT_FALSE(Signature(NodeKind::kCompoundIdentifier, middle) <
               Signature(NodeKind::kIdentifier, left));
  EXPECT_FALSE(Signature(NodeKind::kCompoundIdentifier, middle) <
               Signature(NodeKind::kIdentifier, left));

  // middle vs right
  EXPECT_TRUE(Signature(NodeKind::kIdentifier, middle) < Signature(NodeKind::kIdentifier, right));
  EXPECT_TRUE(Signature(NodeKind::kIdentifier, middle) <
              Signature(NodeKind::kCompoundIdentifier, right));
  EXPECT_TRUE(Signature(NodeKind::kCompoundIdentifier, middle) <
              Signature(NodeKind::kIdentifier, right));
  EXPECT_FALSE(Signature(NodeKind::kIdentifier, right) < Signature(NodeKind::kIdentifier, middle));
  EXPECT_FALSE(Signature(NodeKind::kCompoundIdentifier, right) <
               Signature(NodeKind::kIdentifier, middle));
  EXPECT_FALSE(Signature(NodeKind::kCompoundIdentifier, right) <
               Signature(NodeKind::kIdentifier, middle));

  // self
  EXPECT_TRUE(Signature(NodeKind::kCompoundIdentifier, left) <
              Signature(NodeKind::kIdentifier, left));
  EXPECT_FALSE(Signature(NodeKind::kIdentifier, left) <
               Signature(NodeKind::kCompoundIdentifier, left));
  EXPECT_TRUE(Signature(NodeKind::kCompoundIdentifier, middle) <
              Signature(NodeKind::kIdentifier, middle));
  EXPECT_FALSE(Signature(NodeKind::kIdentifier, middle) <
               Signature(NodeKind::kCompoundIdentifier, middle));
  EXPECT_TRUE(Signature(NodeKind::kCompoundIdentifier, right) <
              Signature(NodeKind::kIdentifier, right));
  EXPECT_FALSE(Signature(NodeKind::kIdentifier, right) <
               Signature(NodeKind::kCompoundIdentifier, right));
}

}  // namespace
