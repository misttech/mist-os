// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zircon/types.h>

#include <vector>

#include <zxtest/zxtest.h>

#include "tools/fidl/fidlc/include/fidl/experimental_flags.h"
#include "tools/fidl/fidlc/include/fidl/flat/sourced.h"
#include "tools/fidl/fidlc/include/fidl/reporter.h"
#include "tools/fidl/fidlc/include/fidl/source_file.h"
#include "tools/fidl/fidlc/include/fidl/transformer.h"
#include "tools/fidl/fidlc/include/fidl/versioning_types.h"
#include "tools/fidl/fidlc/tests/error_test.h"
#include "tools/fidl/fidlc/tests/test_library.h"

namespace {

using namespace fidl;
using namespace fidl::fix;

template <typename T>
void GoodTransform(const std::string& library, std::vector<std::string> deps,
                   const std::string& expect) {
  static_assert(std::is_base_of_v<CompiledTransformer, T>,
                "Only classes derived from |CompiledTransformer| may be used by |GoodTransform()|");
  Reporter reporter;
  ExperimentalFlags experimental_flags;
  VersionSelection version_selection;
  version_selection.Insert(Platform::Parse("example").value(), Version::Head());
  const SourceFile library_source_file = SourceFile("library.fidl", library);
  std::vector<SourceFile> dep_source_files;
  for (size_t i = 0; i < deps.size(); i++) {
    dep_source_files.emplace_back("dep_" + std::to_string(i) + ".fidl", deps[i]);
  }

  std::vector<const SourceFile*> library_input = {&library_source_file};
  std::vector<std::vector<const SourceFile*>> deps_input;
  for (const auto& dep_source_file : dep_source_files) {
    deps_input.push_back({&dep_source_file});
  }

  auto transformer =
      T(library_input, deps_input, &version_selection, experimental_flags, &reporter);
  bool prepared = transformer.Prepare();
  for (const auto& error : transformer.GetErrors()) {
    ADD_FAILURE("transformer failure: %s\n", error.msg.c_str());
  }
  for (const auto& error : reporter.errors()) {
    ADD_FAILURE("reported error: %s\n", error->Format(reporter.program_invocation()).c_str());
  }
  for (const auto& warning : reporter.warnings()) {
    ADD_FAILURE("reported warning: %s\n", warning->Format(reporter.program_invocation()).c_str());
  }
  ASSERT_TRUE(prepared);

  bool transformed = transformer.Transform();
  for (const auto& error : transformer.GetErrors()) {
    ADD_FAILURE("transformer failure: %s\n", error.msg.c_str());
  }
  for (const auto& error : reporter.errors()) {
    ADD_FAILURE("reported error: %s\n", error->Format(reporter.program_invocation()).c_str());
  }
  for (const auto& warning : reporter.warnings()) {
    ADD_FAILURE("reported warning: %s\n", warning->Format(reporter.program_invocation()).c_str());
  }
  ASSERT_TRUE(transformed);

  auto formatted = transformer.Format().value()[0];
  EXPECT_FALSE(transformer.HasErrors());
  EXPECT_TRUE(reporter.errors().empty());
  EXPECT_TRUE(reporter.warnings().empty());
  EXPECT_STREQ(formatted, expect);
}

template <typename T>
void GoodTransform(const std::string& library, const std::string& expect) {
  GoodTransform<T>(library, {}, expect);
}

// A transformer that does nothing, with no dependencies.
class NoopStandaloneTransformer final : public CompiledTransformer {
 public:
  NoopStandaloneTransformer(const std::vector<const SourceFile*>& library_source_files,
                            const ExperimentalFlags& experimental_flags,
                            const VersionSelection* version_selection, Reporter* reporter)
      : CompiledTransformer(library_source_files, version_selection, experimental_flags, reporter) {
  }
};

TEST(CompiledTransformerTests, BadStandaloneCompileFailure) {
  TestLibrary library;
  library.AddSource("bad.fidl", "library example; using missing;");
  ExperimentalFlags experimental_flags;
  VersionSelection version_selection;
  auto transformer = NoopStandaloneTransformer(library.source_files(), experimental_flags,
                                               &version_selection, library.reporter());
  EXPECT_FALSE(transformer.Prepare());
  EXPECT_TRUE(transformer.HasErrors());

  auto errors = transformer.GetErrors();
  ASSERT_EQ(errors.size(), 1);
  ASSERT_EQ(library.reporter()->errors().size(), 1);
  EXPECT_EQ(errors[0].step, fidl::fix::Step::kPreparing);
  EXPECT_ERR(library.reporter()->errors()[0], ErrUnknownLibrary);
  EXPECT_SUBSTR(errors[0].msg, "bad.fidl");
}

// A transformer that does nothing, with dependencies.
class NoopWithDependencyTransformer final : public CompiledTransformer {
 public:
  NoopWithDependencyTransformer(
      const std::vector<const SourceFile*>& library_source_files,
      const std::vector<std::vector<const SourceFile*>>& dependencies_source_files,
      const VersionSelection* version_selection, const ExperimentalFlags& experimental_flags,
      Reporter* reporter)
      : CompiledTransformer(library_source_files, dependencies_source_files, version_selection,
                            experimental_flags, reporter) {}
};

TEST(CompiledTransformerTests, BadDependencyCompileFailure) {
  SharedAmongstLibraries shared;
  Reporter reporter;
  TestLibrary dep(&shared, "dep.fidl", "library dependency; using missing;");
  TestLibrary target(&shared, "target.fidl", "library target; using dependency;");
  ExperimentalFlags experimental_flags;
  VersionSelection version_selection;
  auto transformer =
      NoopWithDependencyTransformer(target.source_files(), {dep.source_files()}, &version_selection,
                                    experimental_flags, &reporter);
  EXPECT_FALSE(transformer.Prepare());
  EXPECT_TRUE(transformer.HasErrors());

  auto errors = transformer.GetErrors();
  ASSERT_EQ(errors.size(), 1);
  ASSERT_EQ(reporter.errors().size(), 1);
  EXPECT_EQ(errors[0].step, fidl::fix::Step::kPreparing);
  EXPECT_ERR(reporter.errors()[0], ErrUnknownLibrary);
  EXPECT_SUBSTR(errors[0].msg, "dep.fidl");
}

const std::string kValuesDependency = R"FIDL(library dependency;
const U8_SINGLE_DIGIT uint8 = 1;
const U8_DOUBLE_DIGIT uint8 = 11;
const U16_SINGLE_DIGIT uint16 = 2;
const U16_DOUBLE_DIGIT uint16 = 22;
const U32_SINGLE_DIGIT uint32 = 3;
const U32_DOUBLE_DIGIT uint32 = 33;
)FIDL";

// A test |CompiledTransformer| derivation that inlines values. It inlines double digit values, but
// leaves single digit values untouched.
class InliningValueTransformer final : public CompiledTransformer {
 public:
  InliningValueTransformer(
      const std::vector<const SourceFile*>& source_files,
      const std::vector<std::vector<const SourceFile*>>& dependencies_source_files,
      const VersionSelection* version_selection, const ExperimentalFlags& experimental_flags,
      Reporter* reporter)
      : CompiledTransformer(source_files, dependencies_source_files, version_selection,
                            experimental_flags, reporter) {}

 private:
  void WhenConstDeclaration(raw::ConstDeclaration* el, TokenSlice& token_slice,
                            const VersionedEntry<flat::Const>* entry) final {
    std::optional<std::string> inlinable = MaybeStringifyValue(entry->Newest()->value->Value());
    if (inlinable.has_value()) {
      // Insert the inlined literal in lieu of the identifier it is replacing in the |token_slice|.
      TokenIterator new_inlined_token_it = InsertInlineConstant(
          token_slice, Token::Kind::kEqual, inlinable.value(), el->constant.get());

      // Update the raw AST to match the |token_slice|.
      std::unique_ptr<raw::Literal> raw_numeric_literal = std::make_unique<raw::NumericLiteral>(
          NewTokenChain(new_inlined_token_it, new_inlined_token_it));
      el->constant = std::make_unique<raw::LiteralConstant>(std::move(raw_numeric_literal));
      token_slice.UpdateTokenPointer(&el->end(), new_inlined_token_it);
    }
  }

  void WhenLayoutParameterList(raw::LayoutParameterList* el, TokenSlice& token_slice,
                               const UniqueEntry<flat::LayoutParameterList>* entry) final {
    // If there is a numeric constraint, it is always last (ie, second).
    if (el->items.size() < 2) {
      return;
    }
    std::optional<std::string> inlinable =
        MaybeStringifyValue(entry->Get()->items[1]->AsConstant()->Value());

    if (inlinable.has_value()) {
      // Insert the inlined literal in lieu of the identifier it is replacing in the |token_slice|.
      TokenIterator new_inlined_token_it = InsertInlineConstant(
          token_slice, Token::Kind::kComma, inlinable.value(), el->items[1].get());

      // Update the raw AST to match the |token_slice|.
      raw::TokenChain chain = NewTokenChain(new_inlined_token_it, new_inlined_token_it);
      std::unique_ptr<raw::Literal> raw_numeric_literal =
          std::make_unique<raw::NumericLiteral>(chain);
      std::unique_ptr<raw::LayoutParameter> raw_inlined_parameter =
          std::make_unique<raw::LiteralLayoutParameter>(
              chain, std::make_unique<raw::LiteralConstant>(std::move(raw_numeric_literal)));
      el->items[1] = std::move(raw_inlined_parameter);
    }
  }

  // Note: only works for multiple constraints, to make this test a bit simpler.
  void WhenTypeConstraints(raw::TypeConstraints* el, TokenSlice& token_slice,
                           const UniqueEntry<flat::TypeConstraints>* entry) final {
    // If there is a numeric constraint, it is always first.
    std::optional<std::string> inlinable = MaybeStringifyValue(entry->Get()->items[0]->Value());
    if (inlinable.has_value()) {
      // Insert the inlined literal in lieu of the identifier it is replacing in the |token_slice|.
      TokenIterator new_inlined_token_it = InsertInlineConstant(
          token_slice, Token::Kind::kLeftAngle, inlinable.value(), el->items[0].get());

      // Update the raw AST to match the |token_slice|.
      std::unique_ptr<raw::Literal> raw_numeric_literal = std::make_unique<raw::NumericLiteral>(
          NewTokenChain(new_inlined_token_it, new_inlined_token_it));
      el->items[0] = std::make_unique<raw::LiteralConstant>(std::move(raw_numeric_literal));
    }
  }

  static TokenIterator InsertInlineConstant(TokenSlice& token_slice, Token::Kind preceding,
                                            std::string inlined_literal,
                                            raw::SourceElement* dropped_constant) {
    std::optional<TokenIterator> maybe_preceding_token_it =
        token_slice.SearchForward([&](const Token* entry) { return entry->kind() == preceding; });
    TokenIterator preceding_token_it = maybe_preceding_token_it.value();

    // Drop the existing constant, and add the new numeric literal in its place.
    token_slice.DropSourceElement(dropped_constant);
    return token_slice.AddTokenAfter(preceding_token_it, inlined_literal,
                                     Token::Kind::kNumericLiteral, Token::Subkind::kNone);
  }

  static std::optional<std::string> MaybeStringifyValue(const flat::ConstantValue& value) {
    uint8_t num = 0;
    switch (value.kind) {
      case fidl::flat::ConstantValue::Kind::kUint8: {
        num = static_cast<const flat::NumericConstantValue<uint8_t>*>(&value)->value;
        break;
      }
      case fidl::flat::ConstantValue::Kind::kUint16: {
        num = static_cast<const flat::NumericConstantValue<uint16_t>*>(&value)->value;
        break;
      }
      case fidl::flat::ConstantValue::Kind::kUint32: {
        num = static_cast<const flat::NumericConstantValue<uint32_t>*>(&value)->value;
        break;
      }
      default:
        return std::nullopt;
    }

    if (num >= 10) {
      return std::to_string(static_cast<int>(num));
    }
    return std::nullopt;
  }
};

TEST(CompiledTransformerTests, InlineConstNoop) {
  std::string noop = R"FIDL(library example;
using dependency;

const A uint8 = dependency.U8_SINGLE_DIGIT;
const B uint16 = dependency.U16_SINGLE_DIGIT;
const C uint32 = dependency.U32_SINGLE_DIGIT;
)FIDL";
  GoodTransform<InliningValueTransformer>(noop, {kValuesDependency}, noop);
}

TEST(CompiledTransformerTests, InlineConstTransformed) {
  GoodTransform<InliningValueTransformer>(
      R"FIDL(library example;
using dependency;

const A uint8 = dependency.U8_DOUBLE_DIGIT;
const B uint16 = dependency.U16_DOUBLE_DIGIT;
const C uint32 = dependency.U32_DOUBLE_DIGIT;
)FIDL",
      {kValuesDependency},
      R"FIDL(library example;
using dependency;

const A uint8 = 11;
const B uint16 = 22;
const C uint32 = 33;
)FIDL");
}

TEST(CompiledTransformerTests, InlineSizeConstraintNoop) {
  std::string noop = R"FIDL(library example;
using dependency;

alias A = vector<uint8>:<dependency.U32_SINGLE_DIGIT, optional>;
)FIDL";
  GoodTransform<InliningValueTransformer>(noop, {kValuesDependency}, noop);
}

TEST(CompiledTransformerTests, InlineSizeConstraintTransformed) {
  GoodTransform<InliningValueTransformer>(
      R"FIDL(library example;
using dependency;

alias A = vector<uint8>:<dependency.U32_DOUBLE_DIGIT, optional>;
)FIDL",
      {kValuesDependency},
      R"FIDL(library example;
using dependency;

alias A = vector<uint8>:<33, optional>;
)FIDL");
}

TEST(CompiledTransformerTests, InlineLayoutParameterNoop) {
  std::string noop = R"FIDL(library example;
using dependency;

alias A = array<uint8, dependency.U32_SINGLE_DIGIT>;
)FIDL";
  GoodTransform<InliningValueTransformer>(noop, {kValuesDependency}, noop);
}

TEST(CompiledTransformerTests, InlineLayoutParameterTransformed) {
  GoodTransform<InliningValueTransformer>(
      R"FIDL(library example;
using dependency;

alias A = array<uint8, dependency.U32_DOUBLE_DIGIT>;
)FIDL",
      {kValuesDependency},
      R"FIDL(library example;
using dependency;

alias A = array<uint8, 33>;
)FIDL");
}

const std::string kDirtyTypeDependency = R"FIDL(library dependency;
@dirty
type Dirty = table {};
)FIDL";
const std::string kIsDirty = "is_dirty";
const std::string kWasDirty = "was_dirty";

// A test |CompiledTransformer| derivation that looks at every declaration member available at
// version 2, and marks it one of two ways: `@is_dirty` if the latest version has a direct (not
// transitive!) child whose type is annotated by the `@dirty` attribute, and `@was_dirty` if a
// previous version of that member did. Two swapped members are considered to be continuations of
// one another if they share the same name.
//
// This test assumes only two versions exist: 1 (the old one) and 2 (the current one).
class DirtyVersionedDeclTransformer final : public CompiledTransformer {
 public:
  DirtyVersionedDeclTransformer(
      const std::vector<const SourceFile*>& source_files,
      const std::vector<std::vector<const SourceFile*>>& dependencies_source_files,
      const VersionSelection* version_selection, const ExperimentalFlags& experimental_flags,
      Reporter* reporter)
      : CompiledTransformer(source_files, dependencies_source_files, version_selection,
                            experimental_flags, reporter) {}

 private:
  void WhenStructDeclaration(raw::Layout*, TokenSlice&, const VersionedEntry<flat::Struct>*) final {
    removed_dirty_members_.clear();
  }
  void WhenTableDeclaration(raw::Layout*, TokenSlice&, const VersionedEntry<flat::Table>*) final {
    removed_dirty_members_.clear();
  }
  void WhenUnionDeclaration(raw::Layout*, TokenSlice&, const VersionedEntry<flat::Union>*) final {
    removed_dirty_members_.clear();
  }

  void WhenStructMember(raw::StructLayoutMember* el, TokenSlice& token_slice,
                        const VersionedEntry<flat::Struct::Member>* entry) final {
    const flat::Struct::Member* versioned = entry->Newest();
    WhenAnyMember(versioned->type_ctor->type, versioned->availability,
                  el->identifier->span().data(), el, el->attributes, token_slice);
  }

  void WhenTableMember(raw::OrdinaledLayoutMember* el, TokenSlice& token_slice,
                       const VersionedEntry<flat::Table::Member>* entry) final {
    const flat::Table::Member* versioned = entry->Newest();
    if (!versioned->maybe_used) {
      return;
    }

    WhenAnyMember(versioned->maybe_used->type_ctor->type, versioned->availability,
                  el->identifier->span().data(), el, el->attributes, token_slice);
  }

  void WhenUnionMember(raw::OrdinaledLayoutMember* el, TokenSlice& token_slice,
                       const VersionedEntry<flat::Union::Member>* entry) final {
    const flat::Union::Member* versioned = entry->Newest();
    if (!versioned->maybe_used) {
      return;
    }

    WhenAnyMember(versioned->maybe_used->type_ctor->type, versioned->availability,
                  el->identifier->span().data(), el, el->attributes, token_slice);
  }

  void WhenAnyMember(const fidl::flat::Type* type, Availability availability,
                     std::string_view identifier, raw::SourceElement* el,
                     std::unique_ptr<raw::AttributeList>& attrs, TokenSlice& token_slice) {
    // Only the newest version (2) gets `@is_dirty` (if it is still using the `Dirty` type) or
    // `@was_dirty` (if a previous version of it was) attributes added. For the purposes of this
    // test, we assume that newer members appear after the removed ones they replaced in the source.
    if (availability.range().Contains(fidl::Version::From(1).value())) {
      if (HasDirtyAttribute(type)) {
        removed_dirty_members_.insert(identifier);
      }
    } else if (availability.range().Contains(fidl::Version::From(2).value())) {
      if (HasDirtyAttribute(type)) {
        Mark(kIsDirty, el, attrs, token_slice);
      }
      if (removed_dirty_members_.find(identifier) != removed_dirty_members_.end()) {
        Mark(kWasDirty, el, attrs, token_slice);
      }
    }
  }

  static void Mark(const std::string& marker, raw::SourceElement* el,
                   std::unique_ptr<raw::AttributeList>& attrs, TokenSlice& token_slice) {
    // Insert the tokens at the head of the |token_slice|.
    TokenIterator at_symbol_token_it = token_slice.AddTokenBefore(
        token_slice.begin(), "@", Token::Kind::kAt, Token::Subkind::kNone);
    TokenIterator attr_name_token_it = token_slice.AddTokenAfter(
        at_symbol_token_it, marker, Token::Kind::kIdentifier, Token::Subkind::kNone);

    // Build the raw AST elements we'll be adding to the tree.
    auto raw_identifier =
        std::make_unique<raw::Identifier>(NewTokenChain(attr_name_token_it, attr_name_token_it));
    auto raw_attribute = std::make_unique<raw::Attribute>(
        NewTokenChain(at_symbol_token_it, attr_name_token_it), std::move(raw_identifier),
        std::vector<std::unique_ptr<raw::AttributeArg>>());

    // Either insert at the head of an existing attribute list, or otherwise create one if it does
    // not exist.
    if (attrs.get() == nullptr) {
      auto raw_attribute_list = std::make_unique<raw::AttributeList>(
          NewTokenChain(at_symbol_token_it, attr_name_token_it),
          std::vector<std::unique_ptr<raw::Attribute>>());
      attrs.swap(raw_attribute_list);
    } else if (!token_slice.UpdateTokenPointer(&attrs->start(), token_slice.begin())) {
      FAIL("could not swap attribute list starting token");
    }
    attrs->attributes.insert(attrs->attributes.begin(), std::move(raw_attribute));

    // Update the element |start()| token in the raw AST to point to the newly added token.
    if (!token_slice.UpdateTokenPointer(&el->start(), token_slice.begin())) {
      FAIL("could not swap versioned element's starting token");
    }
  }

  static bool HasDirtyAttribute(const flat::Type* type) {
    if (type->kind != flat::Type::Kind::kIdentifier) {
      return false;
    }
    auto identifier_type = static_cast<const flat::IdentifierType*>(type);
    flat::Decl* decl = identifier_type->type_decl;
    return decl->attributes->Get("dirty") != nullptr;
  }

  std::set<std::string_view> removed_dirty_members_;
};

TEST(CompiledTransformerTests, StructMemberNoop) {
  GoodTransform<DirtyVersionedDeclTransformer>(
      R"FIDL(@available(added=1)
library example;

type Noop = struct {
    not_dirty bool;
};
)FIDL",
      R"FIDL(@available(added=1)
library example;

type Noop = struct {
    not_dirty bool;
};
)FIDL");
}

TEST(CompiledTransformerTests, StructMemberTransformedUsingCurrentVersion) {
  GoodTransform<DirtyVersionedDeclTransformer>(
      R"FIDL(@available(added=1)
library example;
using dependency;

type MemberShouldBeMarkedIsDirty = struct {
    not_dirty bool;

    @available(added=2)
    is_dirty dependency.Dirty;
};
)FIDL",
      {kDirtyTypeDependency},
      R"FIDL(@available(added=1)
library example;
using dependency;

type MemberShouldBeMarkedIsDirty = struct {
    not_dirty bool;

    @is_dirty
    @available(added=2)
    is_dirty dependency.Dirty;
};
)FIDL");
}

TEST(CompiledTransformerTests, StructMemberTransformedUsingRemovedVersion) {
  GoodTransform<DirtyVersionedDeclTransformer>(
      R"FIDL(@available(added=1)
library example;
using dependency;

type MemberShouldBeMarkedWasDirty = struct {
    @available(removed=2)
    was_dirty dependency.Dirty;

    @available(added=2)
    was_dirty bool;
};
)FIDL",
      {kDirtyTypeDependency},
      R"FIDL(@available(added=1)
library example;
using dependency;

type MemberShouldBeMarkedWasDirty = struct {
    @available(removed=2)
    was_dirty dependency.Dirty;

    @was_dirty
    @available(added=2)
    was_dirty bool;
};
)FIDL");
}

TEST(CompiledTransformerTests, TableMemberNoop) {
  GoodTransform<DirtyVersionedDeclTransformer>(
      R"FIDL(@available(added=1)
library example;

type Noop = table {
    1: not_dirty bool;
};
)FIDL",
      R"FIDL(@available(added=1)
library example;

type Noop = table {
    1: not_dirty bool;
};
)FIDL");
}

TEST(CompiledTransformerTests, TableMemberTransformedUsingCurrentVersion) {
  GoodTransform<DirtyVersionedDeclTransformer>(
      R"FIDL(@available(added=1)
library example;
using dependency;

type MemberShouldBeMarkedIsDirty = table {
    1: not_dirty bool;

    @available(added=2)
    2: is_dirty dependency.Dirty;
};
)FIDL",
      {kDirtyTypeDependency},
      R"FIDL(@available(added=1)
library example;
using dependency;

type MemberShouldBeMarkedIsDirty = table {
    1: not_dirty bool;

    @is_dirty
    @available(added=2)
    2: is_dirty dependency.Dirty;
};
)FIDL");
}

TEST(CompiledTransformerTests, TableMemberTransformedUsingRemovedVersion) {
  GoodTransform<DirtyVersionedDeclTransformer>(
      R"FIDL(@available(added=1)
library example;
using dependency;

type MemberShouldBeMarkedWasDirty = table {
    @available(removed=2)
    1: was_dirty dependency.Dirty;

    @available(added=2)
    1: was_dirty bool;
};
)FIDL",
      {kDirtyTypeDependency},
      R"FIDL(@available(added=1)
library example;
using dependency;

type MemberShouldBeMarkedWasDirty = table {
    @available(removed=2)
    1: was_dirty dependency.Dirty;

    @was_dirty
    @available(added=2)
    1: was_dirty bool;
};
)FIDL");
}

TEST(CompiledTransformerTests, UnionMemberNoop) {
  GoodTransform<DirtyVersionedDeclTransformer>(
      R"FIDL(@available(added=1)
library example;

type Noop = flexible union {
    1: not_dirty bool;
};
)FIDL",
      R"FIDL(@available(added=1)
library example;

type Noop = flexible union {
    1: not_dirty bool;
};
)FIDL");
}

TEST(CompiledTransformerTests, UnionMemberTransformedUsingCurrentVersion) {
  GoodTransform<DirtyVersionedDeclTransformer>(
      R"FIDL(@available(added=1)
library example;
using dependency;

type MemberShouldBeMarkedIsDirty = flexible union {
    1: not_dirty bool;

    @available(added=2)
    2: is_dirty dependency.Dirty;
};
)FIDL",
      {kDirtyTypeDependency},
      R"FIDL(@available(added=1)
library example;
using dependency;

type MemberShouldBeMarkedIsDirty = flexible union {
    1: not_dirty bool;

    @is_dirty
    @available(added=2)
    2: is_dirty dependency.Dirty;
};
)FIDL");
}

TEST(CompiledTransformerTests, UnionMemberTransformedUsingRemovedVersion) {
  GoodTransform<DirtyVersionedDeclTransformer>(
      R"FIDL(@available(added=1)
library example;
using dependency;

type MemberShouldBeMarkedWasDirty = flexible union {
    @available(removed=2)
    1: was_dirty dependency.Dirty;

    @available(added=2)
    1: was_dirty bool;
};
)FIDL",
      {kDirtyTypeDependency},
      R"FIDL(@available(added=1)
library example;
using dependency;

type MemberShouldBeMarkedWasDirty = flexible union {
    @available(removed=2)
    1: was_dirty dependency.Dirty;

    @was_dirty
    @available(added=2)
    1: was_dirty bool;
};
)FIDL");
}

// TODO(fxbug.dev/118371): Add more tests for the |When*| methods that have not yet been covered.

}  // namespace
