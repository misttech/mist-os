// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zircon/types.h>

#include <vector>

#include <zxtest/zxtest.h>

#include "tools/fidl/fidlc/include/fidl/experimental_flags.h"
#include "tools/fidl/fidlc/include/fidl/reporter.h"
#include "tools/fidl/fidlc/include/fidl/source_file.h"
#include "tools/fidl/fidlc/include/fidl/transformer.h"
#include "tools/fidl/fidlc/tests/error_test.h"
#include "tools/fidl/fidlc/tests/test_library.h"

namespace {

using namespace fidl;
using namespace fidl::fix;

template <typename T>
void GoodTransform(const std::string& before, const std::string& after) {
  static_assert(std::is_base_of_v<ParsedTransformer, T>,
                "Only classes derived from |ParsedTransformer| may be used by |GoodTransform()}");

  TestLibrary library(before);
  ExperimentalFlags experimental_flags;
  auto transformer = T(library.source_files(), experimental_flags, library.reporter());
  bool prepared = transformer.Prepare();
  for (const auto& error : transformer.GetErrors()) {
    ADD_FAILURE("transformer failure: %s\n", error.msg.c_str());
  }
  for (const auto& error : library.reporter()->errors()) {
    ADD_FAILURE("reported error: %s\n",
                error->Format(library.reporter()->program_invocation()).c_str());
  }
  for (const auto& warning : library.reporter()->warnings()) {
    ADD_FAILURE("reported warning: %s\n",
                warning->Format(library.reporter()->program_invocation()).c_str());
  }
  ASSERT_TRUE(prepared);

  bool transformed = transformer.Transform();
  for (const auto& error : transformer.GetErrors()) {
    ADD_FAILURE("transformer failure: %s\n", error.msg.c_str());
  }
  for (const auto& error : library.reporter()->errors()) {
    ADD_FAILURE("reported error: %s\n",
                error->Format(library.reporter()->program_invocation()).c_str());
  }
  for (const auto& warning : library.reporter()->warnings()) {
    ADD_FAILURE("reported warning: %s\n",
                warning->Format(library.reporter()->program_invocation()).c_str());
  }
  ASSERT_TRUE(transformed);

  auto formatted = transformer.Format().value()[0];
  EXPECT_FALSE(transformer.HasErrors());
  EXPECT_TRUE(library.reporter()->errors().empty());
  EXPECT_TRUE(library.reporter()->warnings().empty());
  EXPECT_STREQ(formatted, after);
}

// A transformer that does nothing.
class NoopTransformer final : public ParsedTransformer {
 public:
  NoopTransformer(const std::vector<const SourceFile*>& source_files,
                  const ExperimentalFlags& experimental_flags, Reporter* reporter)
      : ParsedTransformer(source_files, experimental_flags, reporter) {}
};

TEST(ParsedTransformerTests, BadOneParsingFailure) {
  TestLibrary library;
  library.AddSource("good.fidl", "library example;");
  library.AddSource("bad.fidl", "library;");
  ExperimentalFlags experimental_flags;
  auto transformer =
      NoopTransformer(library.source_files(), experimental_flags, library.reporter());
  EXPECT_FALSE(transformer.Prepare());
  EXPECT_TRUE(transformer.HasErrors());

  auto errors = transformer.GetErrors();
  ASSERT_EQ(errors.size(), 1);
  ASSERT_EQ(library.reporter()->errors().size(), 1);
  EXPECT_EQ(errors[0].step, fidl::fix::Step::kPreparing);
  EXPECT_ERR(library.reporter()->errors()[0], ErrUnexpectedTokenOfKind);
  EXPECT_SUBSTR(errors[0].msg, "bad.fidl");
}

TEST(ParsedTransformerTests, BadManyParsingFailures) {
  TestLibrary library;
  library.AddSource("bad.fidl", "library;");
  library.AddSource("also_bad.fidl", "library;");
  ExperimentalFlags experimental_flags;
  auto transformer =
      NoopTransformer(library.source_files(), experimental_flags, library.reporter());
  EXPECT_FALSE(transformer.Prepare());
  EXPECT_TRUE(transformer.HasErrors());

  auto errors = transformer.GetErrors();
  ASSERT_EQ(errors.size(), 2);
  ASSERT_EQ(library.reporter()->errors().size(), 2);
  EXPECT_EQ(errors[0].step, fidl::fix::Step::kPreparing);
  EXPECT_ERR(library.reporter()->errors()[0], ErrUnexpectedTokenOfKind);
  EXPECT_SUBSTR(errors[0].msg, "bad.fidl");
  EXPECT_EQ(errors[1].step, fidl::fix::Step::kPreparing);
  EXPECT_ERR(library.reporter()->errors()[1], ErrUnexpectedTokenOfKind);
  EXPECT_SUBSTR(errors[1].msg, "also_bad.fidl");
}

// Immediately fail - used to test transform failure reporting.
class FailingTransformer final : public ParsedTransformer {
 public:
  FailingTransformer(const std::vector<const SourceFile*>& source_files,
                     const ExperimentalFlags& experimental_flags, Reporter* reporter)
      : ParsedTransformer(source_files, experimental_flags, reporter) {}

  void WhenFile(raw::File* el, TokenSlice& token_slice) override {
    // Immediately fail, and do nothing else.
    AddError("failure!");
  }
};

TEST(ParsedTransformerTests, BadOneTransformingFailure) {
  TestLibrary library;
  library.AddSource("valid.fidl", "library example;");
  ExperimentalFlags experimental_flags;
  auto transformer =
      FailingTransformer(library.source_files(), experimental_flags, library.reporter());
  EXPECT_TRUE(transformer.Prepare());
  EXPECT_FALSE(transformer.Transform());
  EXPECT_TRUE(transformer.HasErrors());

  auto errors = transformer.GetErrors();
  ASSERT_EQ(errors.size(), 1);
  ASSERT_TRUE(library.reporter()->errors().empty());
  EXPECT_EQ(errors[0].step, fidl::fix::Step::kTransforming);
  EXPECT_SUBSTR(errors[0].msg, "failure!");
}

TEST(ParsedTransformerTests, BadManyTransformingFailures) {
  TestLibrary library;
  library.AddSource("valid.fidl", "library example;");
  library.AddSource("also_valid.fidl", "library example;");
  ExperimentalFlags experimental_flags;
  auto transformer =
      FailingTransformer(library.source_files(), experimental_flags, library.reporter());
  EXPECT_TRUE(transformer.Prepare());
  EXPECT_FALSE(transformer.Transform());
  EXPECT_TRUE(transformer.HasErrors());

  auto errors = transformer.GetErrors();
  ASSERT_EQ(errors.size(), 2);
  ASSERT_TRUE(library.reporter()->errors().empty());
  EXPECT_EQ(errors[0].step, fidl::fix::Step::kTransforming);
  EXPECT_SUBSTR(errors[0].msg, "failure!");
  EXPECT_EQ(errors[1].step, fidl::fix::Step::kTransforming);
  EXPECT_SUBSTR(errors[1].msg, "failure!");
}

// Adds an errant semicolon. The purpose of this case is for the transformed token list to be
// non-valid FIDL, to test failure at the pretty-printing stage.
class InvalidTransformer final : public ParsedTransformer {
 public:
  InvalidTransformer(const std::vector<const SourceFile*>& source_files,
                     const ExperimentalFlags& experimental_flags, Reporter* reporter)
      : ParsedTransformer(source_files, experimental_flags, reporter) {}

  void WhenLibraryDeclaration(raw::LibraryDeclaration* el, TokenSlice& token_slice) override {
    // Add an (erroneous) extra semicolon.
    token_slice.AddTokenBefore(token_slice.end(), ";", Token::Kind::kSemicolon,
                               Token::Subkind::kNone);
  }
};

TEST(ParsedTransformerTests, BadOneFormattingFailure) {
  TestLibrary library;
  library.AddSource("a.fidl", "library example;");
  ExperimentalFlags experimental_flags;
  auto transformer =
      InvalidTransformer(library.source_files(), experimental_flags, library.reporter());
  EXPECT_TRUE(transformer.Prepare());
  EXPECT_TRUE(transformer.Transform());
  EXPECT_EQ(transformer.Format(), std::nullopt);
  EXPECT_TRUE(transformer.HasErrors());

  auto errors = transformer.GetErrors();
  ASSERT_EQ(errors.size(), 1);
  ASSERT_EQ(library.reporter()->errors().size(), 1);
  EXPECT_EQ(errors[0].step, fidl::fix::Step::kFormatting);
  EXPECT_ERR(library.reporter()->errors()[0], ErrExpectedDeclaration);
  EXPECT_SUBSTR(errors[0].msg, "a.fidl");
}

TEST(ParsedTransformerTests, BadManyFormattingFailures) {
  TestLibrary library;
  library.AddSource("a.fidl", "library example;");
  library.AddSource("b.fidl", "library example;");
  ExperimentalFlags experimental_flags;
  auto transformer =
      InvalidTransformer(library.source_files(), experimental_flags, library.reporter());
  EXPECT_TRUE(transformer.Prepare());
  EXPECT_TRUE(transformer.Transform());
  EXPECT_EQ(transformer.Format(), std::nullopt);
  EXPECT_TRUE(transformer.HasErrors());

  auto errors = transformer.GetErrors();
  ASSERT_EQ(errors.size(), 2);
  ASSERT_EQ(library.reporter()->errors().size(), 2);
  EXPECT_EQ(errors[0].step, fidl::fix::Step::kFormatting);
  EXPECT_ERR(library.reporter()->errors()[0], ErrExpectedDeclaration);
  EXPECT_SUBSTR(errors[0].msg, "a.fidl");
  EXPECT_EQ(errors[1].step, fidl::fix::Step::kFormatting);
  EXPECT_ERR(library.reporter()->errors()[1], ErrExpectedDeclaration);
  EXPECT_SUBSTR(errors[1].msg, "b.fidl");
}

TEST(ParsedTransformerTests, GoodAddLeadingComment) {
  class AddLeadingCommentTransformer final : public ParsedTransformer {
   public:
    AddLeadingCommentTransformer(const std::vector<const SourceFile*>& source_files,
                                 const ExperimentalFlags& experimental_flags, Reporter* reporter)
        : ParsedTransformer(source_files, experimental_flags, reporter) {}

   private:
    void WhenLibraryDeclaration(raw::LibraryDeclaration* el, TokenSlice& token_slice) override {
      // Find the library token.
      std::optional<TokenIterator> maybe_library_token =
          token_slice.SearchForward([](const Token* entry) {
            return entry->kind() == Token::Kind::kIdentifier &&
                   entry->subkind() == Token::Subkind::kLibrary;
          });
      if (!maybe_library_token.has_value()) {
        FAIL("library token missing");
      }

      // Create the new token we'll be inserting. Since it never appears in the raw AST, this is all
      // we need to do.
      TokenIterator library_token_it = maybe_library_token.value();
      token_slice.AddTokenBefore(library_token_it, "// Added\n", Token::Kind::kComment,
                                 Token::Subkind::kNone);
    }
  };

  GoodTransform<AddLeadingCommentTransformer>(
      R"FIDL(library example;
)FIDL",
      R"FIDL(// Added
library example;
)FIDL");
}

TEST(ParsedTransformerTests, GoodDropLeadingComment) {
  class DropLeadingCommentTransformer final : public ParsedTransformer {
   public:
    DropLeadingCommentTransformer(const std::vector<const SourceFile*>& source_files,
                                  const ExperimentalFlags& experimental_flags, Reporter* reporter)
        : ParsedTransformer(source_files, experimental_flags, reporter) {}

   private:
    void WhenFile(raw::File* el, TokenSlice& token_slice) override {
      // Find the comment token.
      std::optional<TokenIterator> maybe_comment_token = token_slice.SearchForward(
          [](const Token* entry) { return entry->kind() == Token::Kind::kComment; });
      if (!maybe_comment_token.has_value()) {
        FAIL("no leading comment");
      }

      // Drop the comment. Since it never appears in the raw AST, this is all we need to do.
      token_slice.DropToken(maybe_comment_token.value());
    }
  };

  GoodTransform<DropLeadingCommentTransformer>(
      R"FIDL(// Removing
library example;
)FIDL",
      R"FIDL(library example;
)FIDL");
}

TEST(ParsedTransformerTests, GoodAddTrailingComment) {
  class AddTrailingCommentTransformer final : public ParsedTransformer {
   public:
    AddTrailingCommentTransformer(const std::vector<const SourceFile*>& source_files,
                                  const ExperimentalFlags& experimental_flags, Reporter* reporter)
        : ParsedTransformer(source_files, experimental_flags, reporter) {}

   private:
    void WhenFile(raw::File* el, TokenSlice& token_slice) override {
      // Create the new token we'll be inserting. Since it never appears in the raw AST, this is all
      // we need to do.
      token_slice.AddTokenBefore(token_slice.end() - 1, "// Added\n", Token::Kind::kComment,
                                 Token::Subkind::kNone);
    }
  };

  GoodTransform<AddTrailingCommentTransformer>(
      R"FIDL(library example;
)FIDL",
      R"FIDL(library example;
// Added
)FIDL");
}

// Even though the logic here is identical to `GoodDropLeadingComment`, this test is subtly
// different in that it does not have to adjust any ordinals post-drop to succeed, whereas the other
// one does.
TEST(ParsedTransformerTests, GoodDropTrailingComment) {
  class DropTrailingCommentTransformer final : public ParsedTransformer {
   public:
    DropTrailingCommentTransformer(const std::vector<const SourceFile*>& source_files,
                                   const ExperimentalFlags& experimental_flags, Reporter* reporter)
        : ParsedTransformer(source_files, experimental_flags, reporter) {}

   private:
    void WhenFile(raw::File* el, TokenSlice& token_slice) override {
      // Find the comment token.
      std::optional<TokenIterator> maybe_comment_token_it = token_slice.SearchForward(
          [](const Token* entry) { return entry->kind() == Token::Kind::kComment; });
      if (!maybe_comment_token_it.has_value()) {
        FAIL("no trailing comment");
      }

      // Drop the comment. Since it never appears in the raw AST, this is all we need to do.
      TokenIterator comment_token_it = maybe_comment_token_it.value();
      token_slice.DropToken(comment_token_it);
    }
  };

  GoodTransform<DropTrailingCommentTransformer>(
      R"FIDL(library example;
// Removing
)FIDL",
      R"FIDL(library example;
)FIDL");
}

TEST(ParsedTransformerTests, GoodAddLeadingSegmentToCompoundIdentifier) {
  class AddLeadingSegmentToCompoundIdentifierTransformer final : public ParsedTransformer {
   public:
    AddLeadingSegmentToCompoundIdentifierTransformer(
        const std::vector<const SourceFile*>& source_files,
        const ExperimentalFlags& experimental_flags, Reporter* reporter)
        : ParsedTransformer(source_files, experimental_flags, reporter) {}

   private:
    void WhenLibraryDeclaration(raw::LibraryDeclaration* el, TokenSlice& token_slice) override {
      std::unique_ptr<raw::CompoundIdentifier>& path = el->path;
      std::unique_ptr<raw::Identifier>& old_leading_identifier = path->components[0];

      // Find the current leading identifier.
      std::optional<TokenIterator> maybe_leading_identifier_it = token_slice.SearchForward(
          [&](const Token* entry) { return entry->span() == old_leading_identifier->span(); });
      if (!maybe_leading_identifier_it.has_value()) {
        FAIL("leading identifier token missing");
      }
      TokenIterator leading_identifier_it = maybe_leading_identifier_it.value();

      // Create the new identifier we'll be inserting.
      TokenIterator new_leading_identifier_it = token_slice.AddTokenBefore(
          leading_identifier_it, "adding", Token::Kind::kIdentifier, Token::Subkind::kNone);

      // Create the dot after the identifier. Because this token will not be held in the raw AST, we
      // don't need to do anything else with it.
      token_slice.AddTokenAfter(new_leading_identifier_it, ".", Token::Kind::kDot,
                                Token::Subkind::kNone);

      // Insert the newly created tokens into this raw AST node.
      auto raw_identifier = std::make_unique<raw::Identifier>(
          NewTokenChain(new_leading_identifier_it, new_leading_identifier_it));
      path->components.insert(path->components.begin(), std::move(raw_identifier));

      // Make sure to update the "start" |Token| of the compound identifier as well!
      if (!token_slice.UpdateTokenPointer(&path->start(), new_leading_identifier_it)) {
        FAIL("could not swap library token");
      }
    }
  };

  GoodTransform<AddLeadingSegmentToCompoundIdentifierTransformer>(
      R"FIDL(library example;
)FIDL",
      R"FIDL(library adding.example;
)FIDL");
}

TEST(ParsedTransformerTests, GoodDropLeadingSegmentFromCompoundIdentifier) {
  class DropLeadingSegmentFromCompoundIdentifierTransformer final : public ParsedTransformer {
   public:
    DropLeadingSegmentFromCompoundIdentifierTransformer(
        const std::vector<const SourceFile*>& source_files,
        const ExperimentalFlags& experimental_flags, Reporter* reporter)
        : ParsedTransformer(source_files, experimental_flags, reporter) {}

   private:
    void WhenLibraryDeclaration(raw::LibraryDeclaration* el, TokenSlice& token_slice) override {
      std::unique_ptr<raw::CompoundIdentifier>& path = el->path;
      std::unique_ptr<raw::Identifier>& leading_identifier = path->components[0];
      std::unique_ptr<raw::Identifier>& new_leading_identifier = path->components[1];

      // Find the period after the leading identifier.
      std::optional<TokenIterator> maybe_leading_identifier_it = token_slice.SearchForward(
          [&](const Token* entry) { return entry->span() == leading_identifier->span(); });
      if (!maybe_leading_identifier_it.has_value()) {
        FAIL("leading identifier token missing");
      }
      TokenIterator leading_identifier_it = maybe_leading_identifier_it.value();
      TokenIterator period_after_leading_identifier_it = leading_identifier_it + 1;

      // Find the period after the new leading identifier.
      std::optional<TokenIterator> maybe_new_leading_identifier_it = token_slice.SearchForward(
          [&](const Token* entry) { return entry->span() == new_leading_identifier->span(); });
      if (!maybe_new_leading_identifier_it.has_value()) {
        FAIL("new leading identifier token missing");
      }
      TokenIterator new_leading_identifier_it = maybe_new_leading_identifier_it.value();

      // Make sure to update the "start" |Token| of the compound identifier.
      if (!token_slice.UpdateTokenPointer(&el->start(), new_leading_identifier_it)) {
        FAIL("could not swap library end token");
      }
      if (!token_slice.UpdateTokenPointer(&path->start(), new_leading_identifier_it)) {
        FAIL("could not swap compound identifier end token");
      }

      // Drop the period, then |raw::Identifier|.
      token_slice.DropToken(period_after_leading_identifier_it);
      token_slice.DropSourceElement(path->components[0].get());
      path->components.erase(path->components.begin());
    }
  };

  GoodTransform<DropLeadingSegmentFromCompoundIdentifierTransformer>(
      R"FIDL(library dropping.example;
)FIDL",
      R"FIDL(library example;
)FIDL");
}

TEST(ParsedTransformerTests, GoodAddMiddleSegmentToCompoundIdentifier) {
  class AddMiddleSegmentToCompoundIdentifierTransformer final : public ParsedTransformer {
   public:
    AddMiddleSegmentToCompoundIdentifierTransformer(
        const std::vector<const SourceFile*>& source_files,
        const ExperimentalFlags& experimental_flags, Reporter* reporter)
        : ParsedTransformer(source_files, experimental_flags, reporter) {}

   private:
    void WhenLibraryDeclaration(raw::LibraryDeclaration* el, TokenSlice& token_slice) override {
      std::unique_ptr<raw::CompoundIdentifier>& path = el->path;

      // Find the dot between the two segments.
      std::optional<TokenIterator> maybe_existing_dot_it = token_slice.SearchForward(
          [&](const Token* entry) { return entry->kind() == Token::Kind::kDot; });
      if (!maybe_existing_dot_it.has_value()) {
        FAIL("dot token missing");
      }
      TokenIterator existing_dot_it = maybe_existing_dot_it.value();

      // Create the new identifier we'll be inserting.
      TokenIterator new_identifier_it = token_slice.AddTokenAfter(
          existing_dot_it, "middle", Token::Kind::kIdentifier, Token::Subkind::kNone);

      // Create the dot after the identifier. Because this token will not be held in the raw AST, we
      // don't need to do anything else with it.
      token_slice.AddTokenAfter(new_identifier_it, ".", Token::Kind::kDot, Token::Subkind::kNone);

      // Insert the newly created tokens into this raw AST node.
      auto raw_identifier =
          std::make_unique<raw::Identifier>(NewTokenChain(new_identifier_it, new_identifier_it));
      path->components.insert(path->components.begin() + 1, std::move(raw_identifier));
    }
  };

  GoodTransform<AddMiddleSegmentToCompoundIdentifierTransformer>(
      R"FIDL(library start.end;
)FIDL",
      R"FIDL(library start.middle.end;
)FIDL");
}

TEST(ParsedTransformerTests, GoodDropMiddleSegmentFromCompoundIdentifier) {
  class DropMiddleSegmentFromCompoundIdentifierTransformer final : public ParsedTransformer {
   public:
    DropMiddleSegmentFromCompoundIdentifierTransformer(
        const std::vector<const SourceFile*>& source_files,
        const ExperimentalFlags& experimental_flags, Reporter* reporter)
        : ParsedTransformer(source_files, experimental_flags, reporter) {}

   private:
    void WhenLibraryDeclaration(raw::LibraryDeclaration* el, TokenSlice& token_slice) override {
      std::unique_ptr<raw::CompoundIdentifier>& path = el->path;
      std::unique_ptr<raw::Identifier>& middle_identifier = path->components[1];

      // Find the period after the middle identifier.
      std::optional<TokenIterator> maybe_middle_identifier_it = token_slice.SearchForward(
          [&](const Token* entry) { return entry->span() == middle_identifier->span(); });
      if (!maybe_middle_identifier_it.has_value()) {
        FAIL("middle identifier token missing");
      }
      TokenIterator middle_identifier_it = maybe_middle_identifier_it.value();
      TokenIterator period_after_middle_identifier_it = middle_identifier_it + 1;

      // Drop the period, then |raw::Identifier|.
      token_slice.DropToken(period_after_middle_identifier_it);
      token_slice.DropSourceElement(path->components[1].get());
      path->components.erase(path->components.begin() + 1);
    }
  };

  GoodTransform<DropMiddleSegmentFromCompoundIdentifierTransformer>(
      R"FIDL(library start.middle.end;
)FIDL",
      R"FIDL(library start.end;
)FIDL");
}

TEST(ParsedTransformerTests, GoodAddTrailingSegmentToCompoundIdentifier) {
  class AddTrailingSegmentToCompoundIdentifierTransformer final : public ParsedTransformer {
   public:
    AddTrailingSegmentToCompoundIdentifierTransformer(
        const std::vector<const SourceFile*>& source_files,
        const ExperimentalFlags& experimental_flags, Reporter* reporter)
        : ParsedTransformer(source_files, experimental_flags, reporter) {}

   private:
    void WhenLibraryDeclaration(raw::LibraryDeclaration* el, TokenSlice& token_slice) override {
      std::unique_ptr<raw::CompoundIdentifier>& path = el->path;
      std::unique_ptr<raw::Identifier>& existing_identifier = path->components[0];

      // Find the existing identifier in the |token_slice|.
      std::optional<TokenIterator> maybe_existing_identifier_it = token_slice.SearchForward(
          [&](const Token* entry) { return entry->span() == existing_identifier->span(); });
      if (!maybe_existing_identifier_it.has_value()) {
        FAIL("existing identifier token missing");
      }
      TokenIterator existing_identifier_it = maybe_existing_identifier_it.value();

      // Create the new dot and identifier we'll be inserting.
      TokenIterator new_dot_it = token_slice.AddTokenAfter(
          existing_identifier_it, ".", Token::Kind::kDot, Token::Subkind::kNone);
      TokenIterator new_trailing_identifier_it = token_slice.AddTokenAfter(
          new_dot_it, "adding", Token::Kind::kIdentifier, Token::Subkind::kNone);

      // Insert the newly created tokens into this raw AST node.
      auto raw_identifier = std::make_unique<raw::Identifier>(
          NewTokenChain(new_trailing_identifier_it, new_trailing_identifier_it));
      path->components.push_back(std::move(raw_identifier));

      // Make sure to update the "end" |Token| of the compound identifier as well!
      if (!token_slice.UpdateTokenPointer(&path->end(), new_trailing_identifier_it)) {
        FAIL("could not swap compound identifier end token");
      }
    }
  };

  GoodTransform<AddTrailingSegmentToCompoundIdentifierTransformer>(
      R"FIDL(library example;
)FIDL",
      R"FIDL(library example.adding;
)FIDL");
}

TEST(ParsedTransformerTests, GoodDropTrailingSegmentFromCompoundIdentifier) {
  class DropTrailingSegmentFromCompoundIdentifierTransformer final : public ParsedTransformer {
   public:
    DropTrailingSegmentFromCompoundIdentifierTransformer(
        const std::vector<const SourceFile*>& source_files,
        const ExperimentalFlags& experimental_flags, Reporter* reporter)
        : ParsedTransformer(source_files, experimental_flags, reporter) {}

   private:
    void WhenLibraryDeclaration(raw::LibraryDeclaration* el, TokenSlice& token_slice) override {
      std::unique_ptr<raw::CompoundIdentifier>& path = el->path;
      std::unique_ptr<raw::Identifier>& trailing_identifier =
          path->components[path->components.size() - 1];
      std::unique_ptr<raw::Identifier>& new_trailing_identifier =
          path->components[path->components.size() - 2];

      // Find the existing trailing identifier in the |token_slice|.
      std::optional<TokenIterator> maybe_trailing_identifier_it = token_slice.SearchForward(
          [&](const Token* entry) { return entry->span() == trailing_identifier->span(); });
      if (!maybe_trailing_identifier_it.has_value()) {
        FAIL("trailing identifier token missing");
      }
      TokenIterator trailing_identifier_it = maybe_trailing_identifier_it.value();
      TokenIterator period_before_trailing_identifier_it = trailing_identifier_it - 1;

      // Find the period after the new trailing identifier.
      std::optional<TokenIterator> maybe_new_trailing_identifier_it = token_slice.SearchForward(
          [&](const Token* entry) { return entry->span() == new_trailing_identifier->span(); });
      if (!maybe_new_trailing_identifier_it.has_value()) {
        FAIL("new trailing identifier token missing");
      }
      TokenIterator new_trailing_identifier_it = maybe_new_trailing_identifier_it.value();

      // Make sure to update the "end" |Token| of the compound identifier.
      if (!token_slice.UpdateTokenPointer(&el->end(), new_trailing_identifier_it)) {
        FAIL("could not swap library end token");
      }
      if (!token_slice.UpdateTokenPointer(&path->end(), new_trailing_identifier_it)) {
        FAIL("could not swap compound identifier end token");
      }

      // Drop the period, then |raw::Identifier|.
      token_slice.DropToken(period_before_trailing_identifier_it);
      token_slice.DropSourceElement(path->components[path->components.size() - 1].get());
      path->components.erase(path->components.end() - 1);
    }
  };

  GoodTransform<DropTrailingSegmentFromCompoundIdentifierTransformer>(
      R"FIDL(library example.dropping;
)FIDL",
      R"FIDL(library example;
)FIDL");
}

TEST(ParsedTransformerTests, GoodAddAttributeToDecl) {
  class AddAttributeToDeclTransformer final : public ParsedTransformer {
   public:
    AddAttributeToDeclTransformer(const std::vector<const SourceFile*>& source_files,
                                  const ExperimentalFlags& experimental_flags, Reporter* reporter)
        : ParsedTransformer(source_files, experimental_flags, reporter) {}

   private:
    void WhenLibraryDeclaration(raw::LibraryDeclaration* el, TokenSlice& token_slice) override {
      // Find the library token.
      std::optional<TokenIterator> maybe_library_token =
          token_slice.SearchForward([](const Token* entry) {
            return entry->kind() == Token::Kind::kIdentifier &&
                   entry->subkind() == Token::Subkind::kLibrary;
          });
      if (!maybe_library_token.has_value()) {
        FAIL("library token missing");
      }

      // Create the new tokens we'll be inserting.
      TokenIterator library_token_it = maybe_library_token.value();
      TokenIterator at_it = token_slice.AddTokenBefore(library_token_it, "@", Token::Kind::kAt,
                                                       Token::Subkind::kNone);
      TokenIterator attr_it =
          token_slice.AddTokenAfter(at_it, "attr", Token::Kind::kIdentifier, Token::Subkind::kNone);

      // Insert the newly created tokens into this raw AST node.
      auto raw_identifier = std::make_unique<raw::Identifier>(NewTokenChain(attr_it, attr_it));
      auto raw_attribute =
          std::make_unique<raw::Attribute>(NewTokenChain(at_it, attr_it), std::move(raw_identifier),
                                           std::vector<std::unique_ptr<raw::AttributeArg>>());
      std::vector<std::unique_ptr<raw::Attribute>> raw_attributes;
      raw_attributes.push_back(std::move(raw_attribute));
      el->attributes = std::make_unique<raw::AttributeList>(NewTokenChain(at_it, attr_it),
                                                            std::move(raw_attributes));

      // Make sure to update the "start" |Token| of the library as well!
      if (!token_slice.UpdateTokenPointer(&el->start(), at_it)) {
        FAIL("could not swap library token");
      }
    }
  };

  GoodTransform<AddAttributeToDeclTransformer>(
      R"FIDL(library example;
)FIDL",
      R"FIDL(@attr
library example;
)FIDL");
}

TEST(ParsedTransformerTests, GoodDropAttributeFromDecl) {
  class DropAttributeFromDeclTransformer final : public ParsedTransformer {
   public:
    DropAttributeFromDeclTransformer(const std::vector<const SourceFile*>& source_files,
                                     const ExperimentalFlags& experimental_flags,
                                     Reporter* reporter)
        : ParsedTransformer(source_files, experimental_flags, reporter) {}

   private:
    void WhenLibraryDeclaration(raw::LibraryDeclaration* el, TokenSlice& token_slice) override {
      // Find the library token.
      std::optional<TokenIterator> maybe_library_token =
          token_slice.SearchForward([](const Token* entry) {
            return entry->kind() == Token::Kind::kIdentifier &&
                   entry->subkind() == Token::Subkind::kLibrary;
          });
      if (!maybe_library_token.has_value()) {
        FAIL("library token missing");
      }

      // This |raw::Library| node will no longer |start()| with `@` of the attribute, since that
      // token is about to be dropped. Point it to the correct (existing) token that we'll want here
      // once the |raw::AttributeList| is removed: `library`.
      TokenIterator library_token_it = maybe_library_token.value();
      token_slice.UpdateTokenPointer(&el->start(), library_token_it);

      // The |raw::AttributeList| is now safe to drop.
      token_slice.DropSourceElement(el->attributes.get());
      el->attributes = nullptr;
    }
  };

  GoodTransform<DropAttributeFromDeclTransformer>(
      R"FIDL(
@attr
library example;
)FIDL",
      R"FIDL(library example;
)FIDL");
}

TEST(ParsedTransformerTests, GoodAddModifierToTypeDecl) {
  class AddModifierToTypeDeclTransformer final : public ParsedTransformer {
   public:
    AddModifierToTypeDeclTransformer(const std::vector<const SourceFile*>& source_files,
                                     const ExperimentalFlags& experimental_flags,
                                     Reporter* reporter)
        : ParsedTransformer(source_files, experimental_flags, reporter) {}

   private:
    // It is best practice to use the "highest" (aka closest to the root) point in the AST where the
    // token we're interested could be referenced at. In our case, the `resource` modifier can be
    // the |start()| token of the |raw::TypeConstructor|, so we do the transform there.
    void WhenTypeConstructor(raw::TypeConstructor* el, TokenSlice& token_slice) override {
      if (!found_match_) {
        return;
      }
      found_match_ = false;
      raw::InlineLayoutReference* inline_ref =
          static_cast<raw::InlineLayoutReference*>(el->layout_ref.get());
      std::unique_ptr<raw::Layout>& layout = inline_ref->layout;

      // Find the `union`/`struct` token in the |token_slice|.
      std::optional<TokenIterator> maybe_layout_token_it =
          token_slice.SearchForward([](const Token* entry) {
            return entry->kind() == Token::Kind::kIdentifier &&
                   (entry->subkind() == Token::Subkind::kUnion ||
                    entry->subkind() == Token::Subkind::kStruct);
          });
      if (!maybe_layout_token_it.has_value()) {
        FAIL("layout kind token missing");
      }
      TokenIterator layout_token_it = maybe_layout_token_it.value();

      // Do we already have modifiers?
      if (layout->modifiers == nullptr) {
        // No modifiers - make them from scratch! Create the new token we'll be inserting.
        TokenIterator resource_token_it = token_slice.AddTokenBefore(
            layout_token_it, "resource", Token::Kind::kIdentifier, Token::Subkind::kResource);

        // Create the new |raw::Modifiers|.
        auto modifier = std::make_optional<raw::Modifier<types::Resourceness>>(
            types::Resourceness::kResource, **resource_token_it);
        layout->modifiers = std::make_unique<raw::Modifiers>(
            NewTokenChain(resource_token_it, resource_token_it), modifier, std::nullopt, true);

        // Point the relevant start pointers in all the raw AST nodes that start with the modifiers
        // to the correct location.
        token_slice.UpdateTokenPointer(&el->start(), resource_token_it);
        token_slice.UpdateTokenPointer(&inline_ref->start(), resource_token_it);
        token_slice.UpdateTokenPointer(&layout->start(), resource_token_it);
        return;
      }

      // Modifiers already exist! Make sure this isn't already a `resource`.
      std::unique_ptr<raw::Modifiers>& modifiers = layout->modifiers;
      std::optional<TokenIterator> maybe_resource_token =
          token_slice.SearchForward([](const Token* entry) {
            return entry->kind() == Token::Kind::kIdentifier &&
                   entry->subkind() == Token::Subkind::kResource;
          });
      if (maybe_resource_token.has_value()) {
        return;
      }

      // Create the new token we'll be inserting.
      TokenIterator existing_modifier_token_it = layout_token_it - 1;
      TokenIterator resource_token_it =
          token_slice.AddTokenAfter(existing_modifier_token_it, "resource",
                                    Token::Kind::kIdentifier, Token::Subkind::kResource);

      // Add the new token to the existing |raw::Modifiers|.
      modifiers->maybe_resourceness = std::make_optional<raw::Modifier<types::Resourceness>>(
          types::Resourceness::kResource, **resource_token_it);

      // Point the end pointer in the |token_slice| to the correct location.
      token_slice.UpdateTokenPointer(&modifiers->end(), resource_token_it);
    }

    void WhenStructDeclaration(raw::Layout* el, TokenSlice& token_slice) override {
      found_match_ = true;
    }

    void WhenUnionDeclaration(raw::Layout* el, TokenSlice& token_slice) override {
      found_match_ = true;
    }

    bool found_match_ = false;
  };

  GoodTransform<AddModifierToTypeDeclTransformer>(
      R"FIDL(library example;

type U = union {};
)FIDL",
      R"FIDL(library example;

type U = resource union {};
)FIDL");

  GoodTransform<AddModifierToTypeDeclTransformer>(
      R"FIDL(library example;

type U = flexible union {};
)FIDL",
      R"FIDL(library example;

type U = flexible resource union {};
)FIDL");

  GoodTransform<AddModifierToTypeDeclTransformer>(
      R"FIDL(library example;

type U = resource union {};
)FIDL",
      R"FIDL(library example;

type U = resource union {};
)FIDL");

  // Structs work just like unions.
  GoodTransform<AddModifierToTypeDeclTransformer>(
      R"FIDL(library example;

type S = struct {};
)FIDL",
      R"FIDL(library example;

type S = resource struct {};
)FIDL");

  GoodTransform<AddModifierToTypeDeclTransformer>(
      R"FIDL(library example;

type S = resource struct {};
)FIDL",
      R"FIDL(library example;

type S = resource struct {};
)FIDL");

  // Tables are completely ignored by this transformer.
  GoodTransform<AddModifierToTypeDeclTransformer>(
      R"FIDL(library example;

type T = table {};
)FIDL",
      R"FIDL(library example;

type T = table {};
)FIDL");

  // Multiple instances.
  GoodTransform<AddModifierToTypeDeclTransformer>(
      R"FIDL(library example;

type U = flexible union {};
type S = struct {};
)FIDL",
      R"FIDL(library example;

type U = flexible resource union {};
type S = resource struct {};
)FIDL");
}

TEST(ParsedTransformerTests, GoodDropModifierFromTypeDecl) {
  class DropModifierFromTypeDeclTransformer final : public ParsedTransformer {
   public:
    DropModifierFromTypeDeclTransformer(const std::vector<const SourceFile*>& source_files,
                                        const ExperimentalFlags& experimental_flags,
                                        Reporter* reporter)
        : ParsedTransformer(source_files, experimental_flags, reporter) {}

   private:
    // It is best practice to use the "highest" (aka closest to the root) point in the AST where the
    // token we're interested could be referenced at. In our case, the `resource` modifier can be
    // the |start()| token of the |raw::TypeConstructor|, so we do the transform there.
    void WhenTypeConstructor(raw::TypeConstructor* el, TokenSlice& token_slice) override {
      if (!found_match_) {
        return;
      }
      found_match_ = false;
      raw::InlineLayoutReference* inline_ref =
          static_cast<raw::InlineLayoutReference*>(el->layout_ref.get());
      std::unique_ptr<raw::Layout>& layout = inline_ref->layout;

      // Find the `union`/`struct` token.
      std::optional<TokenIterator> maybe_layout_token =
          token_slice.SearchForward([](const Token* entry) {
            return entry->kind() == Token::Kind::kIdentifier &&
                   (entry->subkind() == Token::Subkind::kUnion ||
                    entry->subkind() == Token::Subkind::kStruct);
          });
      if (!maybe_layout_token.has_value()) {
        FAIL("union token missing");
      }
      TokenIterator layout_token_it = maybe_layout_token.value();

      // Make sure the `resource` modifier exists.
      if (layout->modifiers == nullptr) {
        return;
      }
      std::unique_ptr<raw::Modifiers>& modifiers = layout->modifiers;
      std::optional<TokenIterator> maybe_resource_token_it =
          token_slice.SearchForward([](const Token* entry) {
            return entry->kind() == Token::Kind::kIdentifier &&
                   entry->subkind() == Token::Subkind::kResource;
          });
      if (!maybe_resource_token_it.has_value()) {
        return;
      }

      // If we have other modifiers, we'll need to keep the |raw::Modifiers| around - otherwise, we
      // can just drop it in its entirety.
      if (modifiers->maybe_strictness == std::nullopt) {
        // This |raw::Layout| node will no longer |start()| with `resource` modifier, since that
        // token is about to be dropped. Point it to the correct (existing) token that we'll want
        // here once the |raw::Modifiers| is removed.
        token_slice.UpdateTokenPointer(&layout->start(), layout_token_it);

        // The |raw::Modifiers| is now safe to drop.
        token_slice.DropSourceElement(layout->modifiers.get());
        layout->modifiers = nullptr;
        return;
      }

      std::optional<TokenIterator> maybe_strictness_token_it =
          token_slice.SearchForward([](const Token* entry) {
            return entry->kind() == Token::Kind::kIdentifier &&
                   (entry->subkind() == Token::Subkind::kFlexible ||
                    entry->subkind() == Token::Subkind::kStrict);
          });
      if (!maybe_strictness_token_it.has_value()) {
        FAIL("strictness token missing");
      }
      TokenIterator strictness_token_it = maybe_strictness_token_it.value();

      // Is the `resource` token first or last?
      TokenIterator resource_token_it = maybe_resource_token_it.value();
      if (resource_token_it + 1 == layout_token_it) {
        // This |raw::Modifiers| node will no longer |end()| with the `resource` modifier, since
        // that token is about to be dropped. Point it to the correct (existing) token that we'll
        // want here once the |raw::Modifier| is removed.
        token_slice.UpdateTokenPointer(&modifiers->end(), strictness_token_it);

        // The |raw::Modifier| is now safe to drop.
        token_slice.DropToken(resource_token_it);
        modifiers->maybe_strictness = std::nullopt;
        return;
      }

      // This |raw::Modifiers| node will no longer |start()| with the `resource` modifier, since
      // that token is about to be dropped. Point it to the correct (existing) token that we'll want
      // here once the |raw::Modifier| is removed.
      token_slice.UpdateTokenPointer(&modifiers->start(), strictness_token_it);

      // Do the same for the other raw AST nodes.
      token_slice.UpdateTokenPointer(&el->start(), strictness_token_it);
      token_slice.UpdateTokenPointer(&inline_ref->start(), strictness_token_it);
      token_slice.UpdateTokenPointer(&layout->start(), strictness_token_it);

      // The |raw::Modifier| is now safe to drop.
      token_slice.DropToken(resource_token_it);
      modifiers->maybe_strictness = std::nullopt;
    }

    void WhenStructDeclaration(raw::Layout* el, TokenSlice& token_slice) override {
      found_match_ = true;
    }

    void WhenUnionDeclaration(raw::Layout* el, TokenSlice& token_slice) override {
      found_match_ = true;
    }

    bool found_match_ = false;
  };

  GoodTransform<DropModifierFromTypeDeclTransformer>(
      R"FIDL(library example;

type U = resource union {};
)FIDL",
      R"FIDL(library example;

type U = union {};
)FIDL");

  GoodTransform<DropModifierFromTypeDeclTransformer>(
      R"FIDL(library example;

type U = flexible resource union {};
)FIDL",
      R"FIDL(library example;

type U = flexible union {};
)FIDL");

  GoodTransform<DropModifierFromTypeDeclTransformer>(
      R"FIDL(library example;

type U = resource flexible union {};
)FIDL",
      R"FIDL(library example;

type U = flexible union {};
)FIDL");

  GoodTransform<DropModifierFromTypeDeclTransformer>(
      R"FIDL(library example;

type U = union {};
)FIDL",
      R"FIDL(library example;

type U = union {};
)FIDL");

  GoodTransform<DropModifierFromTypeDeclTransformer>(
      R"FIDL(library example;

type U = flexible union {};
)FIDL",
      R"FIDL(library example;

type U = flexible union {};
)FIDL");

  // Structs work just like unions.
  GoodTransform<DropModifierFromTypeDeclTransformer>(
      R"FIDL(library example;

type S = resource struct {};
)FIDL",
      R"FIDL(library example;

type S = struct {};
)FIDL");

  GoodTransform<DropModifierFromTypeDeclTransformer>(
      R"FIDL(library example;

type S = struct {};
)FIDL",
      R"FIDL(library example;

type S = struct {};
)FIDL");

  // Tables are completely ignored by this transformer.
  GoodTransform<DropModifierFromTypeDeclTransformer>(
      R"FIDL(library example;

type T = table {};
)FIDL",
      R"FIDL(library example;

type T = table {};
)FIDL");

  // Multiple instances.
  GoodTransform<DropModifierFromTypeDeclTransformer>(
      R"FIDL(library example;

type U = resource flexible union {};
type S = resource struct {};
)FIDL",
      R"FIDL(library example;

type U = flexible union {};
type S = struct {};
)FIDL");
}

TEST(ParsedTransformerTests, GoodSwapModifierInTypeDecl) {
  class SwapModifierInTypeDeclTransformer final : public ParsedTransformer {
   public:
    SwapModifierInTypeDeclTransformer(const std::vector<const SourceFile*>& source_files,
                                      const ExperimentalFlags& experimental_flags,
                                      Reporter* reporter)
        : ParsedTransformer(source_files, experimental_flags, reporter) {}

   private:
    // It is best practice to use the "highest" (aka closest to the root) point in the AST where the
    // token we're interested could be referenced at. In our case, the `resource` modifier can be
    // the |start()| token of the |raw::TypeConstructor|, so we do the transform there.
    void WhenTypeConstructor(raw::TypeConstructor* el, TokenSlice& token_slice) override {
      if (!found_match_) {
        return;
      }
      found_match_ = false;
      raw::InlineLayoutReference* inline_ref =
          static_cast<raw::InlineLayoutReference*>(el->layout_ref.get());
      std::unique_ptr<raw::Layout>& layout = inline_ref->layout;

      // Nothing to do if there is no existing strictness modifier.
      std::unique_ptr<raw::Modifiers>& modifiers = layout->modifiers;
      if (modifiers == nullptr) {
        return;
      }
      std::optional<raw::Modifier<types::Strictness>> strictness_modifier =
          modifiers->maybe_strictness;
      if (!strictness_modifier.has_value()) {
        return;
      }

      // Find the existing strictness token.
      std::optional<TokenIterator> maybe_old_strictness_token_it =
          token_slice.SearchForward([](const Token* entry) {
            return entry->kind() == Token::Kind::kIdentifier &&
                   (entry->subkind() == Token::Subkind::kFlexible ||
                    entry->subkind() == Token::Subkind::kStrict);
          });
      if (!maybe_old_strictness_token_it.has_value()) {
        FAIL("strictness token missing");
      }
      TokenIterator old_strictness_token_it = maybe_old_strictness_token_it.value();

      // Add the new "inverted" strictness token after it.
      TokenIterator new_strictness_token_it;
      if (modifiers->maybe_strictness.value().value == types::Strictness::kFlexible) {
        new_strictness_token_it = token_slice.AddTokenAfter(
            old_strictness_token_it, "strict", Token::Kind::kIdentifier, Token::Subkind::kStrict);
      } else {
        new_strictness_token_it =
            token_slice.AddTokenAfter(old_strictness_token_it, "flexible", Token::Kind::kIdentifier,
                                      Token::Subkind::kFlexible);
      }

      // Is there a `resource` modifier as well? If it comes first, we don't need to update any
      // token pointers except for the |end()| of the |raw::Modifiers| list itself.
      if (modifiers->maybe_resourceness.has_value() && modifiers->resourceness_comes_first) {
        token_slice.UpdateTokenPointer(&modifiers->end(), new_strictness_token_it);
        token_slice.DropToken(old_strictness_token_it);
        return;
      }

      // This |raw::Modifiers| node will no longer |start()| with the old strictness modifier, since
      // that token is about to be dropped. Point it to the correct (newly added) token that we'll
      // want here once the old |raw::Modifier| is removed. Do the same for the other raw AST nodes.
      token_slice.UpdateTokenPointer(&modifiers->start(), new_strictness_token_it);
      token_slice.UpdateTokenPointer(&el->start(), new_strictness_token_it);
      token_slice.UpdateTokenPointer(&inline_ref->start(), new_strictness_token_it);
      token_slice.UpdateTokenPointer(&layout->start(), new_strictness_token_it);

      // The outgoing |raw::Modifier| is now safe to drop.
      token_slice.DropToken(old_strictness_token_it);
    }

    void WhenUnionDeclaration(raw::Layout* el, TokenSlice& token_slice) override {
      found_match_ = true;
    }

    bool found_match_ = false;
  };

  // All `strict` -> `flexible` combinations.
  GoodTransform<SwapModifierInTypeDeclTransformer>(
      R"FIDL(library example;

type U = resource strict union {};
)FIDL",
      R"FIDL(library example;

type U = resource flexible union {};
)FIDL");

  GoodTransform<SwapModifierInTypeDeclTransformer>(
      R"FIDL(library example;

type U = strict resource union {};
)FIDL",
      R"FIDL(library example;

type U = flexible resource union {};
)FIDL");

  GoodTransform<SwapModifierInTypeDeclTransformer>(
      R"FIDL(library example;

type U = strict union {};
)FIDL",
      R"FIDL(library example;

type U = flexible union {};
)FIDL");

  // All `flexible` -> `strict` combinations.
  GoodTransform<SwapModifierInTypeDeclTransformer>(
      R"FIDL(library example;

type U = flexible union {};
)FIDL",
      R"FIDL(library example;

type U = strict union {};
)FIDL");

  GoodTransform<SwapModifierInTypeDeclTransformer>(
      R"FIDL(library example;

type U = resource flexible union {};
)FIDL",
      R"FIDL(library example;

type U = resource strict union {};
)FIDL");

  GoodTransform<SwapModifierInTypeDeclTransformer>(
      R"FIDL(library example;

type U = flexible resource union {};
)FIDL",
      R"FIDL(library example;

type U = strict resource union {};
)FIDL");

  // No-op cases.
  GoodTransform<SwapModifierInTypeDeclTransformer>(
      R"FIDL(library example;

type U = resource union {};
)FIDL",
      R"FIDL(library example;

type U = resource union {};
)FIDL");

  GoodTransform<SwapModifierInTypeDeclTransformer>(
      R"FIDL(library example;

type U = union {};
)FIDL",
      R"FIDL(library example;

type U = union {};
)FIDL");

  // Multiple instances.
  GoodTransform<SwapModifierInTypeDeclTransformer>(
      R"FIDL(library example;

type U1 = strict union {};
type U2 = flexible union {};
)FIDL",
      R"FIDL(library example;

type U1 = flexible union {};
type U2 = strict union {};
)FIDL");
}

// TODO(fxbug.dev/118371): We should add the following tests for completeness.
//   * BadManyParsingFailuresInSingleFile
//   * GoodAddConstraintToTypeConstructor
//   * GoodDropConstraintFromTypeConstructor
//   * GoodAddWrappedTypeToTypeDecl
//   * GoodDropWrappedTypeFromTypeDecl
//   * GoodAddDeclarationToFile
//   * GoodDropDeclarationFromFile
//   * GoodAddMemberToDeclaration
//   * GoodDropMemberFromDeclaration

}  // namespace
