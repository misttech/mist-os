// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fit/function.h>
#include <zircon/assert.h>

#include "tools/fidl/fidlc/include/fidl/source_map_generator.h"
#include "tools/fidl/fidlc/include/fidl/transformer.h"
#include "tools/fidl/fidlc/include/fidl/tree_visitor.h"

// A macro containing the logic of calling any particular user-supplied |When*| function, which
// should be the same regardless of the particular |NodeKind| being visited.
//
// What's happening here:
//   1. Visit all children first (ie, do post-order traversal of the raw AST).
//   2. Search the |MutableElementMap| to find the copy of the node we're currently visiting in the
//      mutable raw AST.
//   3. Search the |SourceMap| to find all of the flat AST nodes associated with the one we are
//      currently visiting.
//   4. Get a slice into the |Token|s covered by that raw AST node, inclusive of that node's
//      |start()| and |end()| tokens.
//   5. Create a |TokenPointerResolver|, so that we can do a bit of automated repair after the
//      user-supplied |When*| method has completed.
//   6. Call the user-supplied |When*| logic.
//   7. Cleanup using the |TokenPointerResolver|.
//
#define TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP(RAW_AST_NODE, WHEN, ENTRY, FLAT_AST_NODE) \
  DeclarationOrderTreeVisitor::On##RAW_AST_NODE(el);                                   \
  auto mut = static_cast<raw::RAW_AST_NODE*>(GetAsMutable(el.get()));                  \
  auto comp = source_map_->Get##ENTRY<flat::FLAT_AST_NODE>(el->source_signature());    \
  TokenSlice token_slice = GetTokenSlice(el->start(), el->end());                      \
  internal::TokenPointerResolver resolver = GetTokenPointerResolver(&token_slice);     \
  When##WHEN(mut, token_slice, comp);                                                  \
  std::unique_ptr<raw::RAW_AST_NODE> uptr(mut);                                        \
  resolver.On##RAW_AST_NODE(uptr);                                                     \
  static_cast<void>(uptr.release());

// A few visitor methods defined by |TreeVisitor| supply const references, rather than const
// |unique_ptr<...>| references, as their sole parameter. This macro does the same thing as
// |TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP| for such cases.
#define TRANSFORM_REFERENCE_WITH_SOURCE_MAP(RAW_AST_NODE, FLAT_AST_NODE)           \
  DeclarationOrderTreeVisitor::On##RAW_AST_NODE(el);                               \
  auto mut = static_cast<raw::RAW_AST_NODE*>(GetAsMutable(&el));                   \
  auto comp = source_map_->GetUnique<flat::FLAT_AST_NODE>(el.source_signature());  \
  TokenSlice token_slice = GetTokenSlice(el.start(), el.end());                    \
  internal::TokenPointerResolver resolver = GetTokenPointerResolver(&token_slice); \
  When##RAW_AST_NODE(mut, token_slice, comp);                                      \
  resolver.On##RAW_AST_NODE(*mut);

namespace fidl::fix {

bool CompiledTransformer::CompileLibrary(const std::vector<const SourceFile*>& source_files,
                                         const fidl::ExperimentalFlags& experimental_flags,
                                         fidl::flat::Compiler* compiler, Reporter* reporter) {
  for (const auto& source_file : source_files) {
    std::unique_ptr<raw::File> ast = ParseSource(source_file, experimental_flags, reporter);
    if (ast == nullptr) {
      return false;
    }
    if (!compiler->ConsumeFile(std::move(ast))) {
      return false;
    }
  }

  if (!compiler->Compile()) {
    return false;
  }
  return true;
}

bool CompiledTransformer::Prepare() {
  ZX_ASSERT(step() == Step::kNew);
  NextStep();

  if (source_files_.empty()) {
    AddError("No source files provided\n");
    return false;
  }
  BuildTransformStates();

  // Compile all of the dependencies.
  for (const auto& source_file_set : dependencies_source_files_) {
    if (source_file_set.empty()) {
      continue;
    }

    fidl::flat::Compiler compiler(&all_libraries_, version_selection_,
                                  fidl::ordinals::GetGeneratedOrdinal64, experimental_flags_);
    if (!CompileLibrary(source_file_set, experimental_flags_, &compiler, reporter())) {
      AddError("Failed to compile dependency at least partially contained in " +
               (std::string(source_file_set[0]->filename())) + "\n");
      return false;
    }
  }
  if (HasErrors()) {
    return false;
  }

  // Compile the target library.
  fidl::flat::Compiler compiler(&all_libraries_, version_selection_,
                                fidl::ordinals::GetGeneratedOrdinal64, experimental_flags_);
  if (!CompileLibrary(source_files_, experimental_flags_, &compiler, reporter())) {
    AddError("Failed to compile library at least partially contained in " +
             (std::string(source_files_[0]->filename())) + "\n");
    return false;
  }
  if (HasErrors()) {
    return false;
  }
  if (all_libraries_.Empty()) {
    AddError("No library was produced\n");
    return false;
  }

  // Build a |SourceMap| for the target library.
  const fidl::flat::Library* library = all_libraries_.target_library();
  ZX_ASSERT(library != nullptr);
  source_map_ =
      std::make_unique<SourceMap>(SourceMapGenerator(library, experimental_flags_).Produce());

  NextStep();
  return true;
}

void CompiledTransformer::OnAliasDeclaration(const std::unique_ptr<raw::AliasDeclaration>& el) {
  TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP(AliasDeclaration, AliasDeclaration, Versioned, Alias);
}

void CompiledTransformer::OnAttributeArg(const std::unique_ptr<raw::AttributeArg>& el) {
  TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP(AttributeArg, AttributeArg, Unique, AttributeArg);
}

void CompiledTransformer::OnAttribute(const std::unique_ptr<raw::Attribute>& el) {
  TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP(Attribute, Attribute, Unique, Attribute);
}

void CompiledTransformer::OnBinaryOperatorConstant(
    const std::unique_ptr<raw::BinaryOperatorConstant>& el) {
  TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP(BinaryOperatorConstant, BinaryOperatorConstant, Unique,
                                       BinaryOperatorConstant);
}

void CompiledTransformer::OnBoolLiteral(raw::BoolLiteral& el) {
  TRANSFORM_REFERENCE_WITH_SOURCE_MAP(BoolLiteral, LiteralConstant);
}

void CompiledTransformer::OnConstDeclaration(const std::unique_ptr<raw::ConstDeclaration>& el) {
  TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP(ConstDeclaration, ConstDeclaration, Versioned, Const);
}

void CompiledTransformer::OnDocCommentLiteral(raw::DocCommentLiteral& el) {
  TRANSFORM_REFERENCE_WITH_SOURCE_MAP(DocCommentLiteral, LiteralConstant);
}

void CompiledTransformer::OnIdentifierConstant(const std::unique_ptr<raw::IdentifierConstant>& el) {
  TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP(IdentifierConstant, IdentifierConstant, Unique,
                                       IdentifierConstant);
}

void CompiledTransformer::OnIdentifierLayoutParameter(
    const std::unique_ptr<raw::IdentifierLayoutParameter>& el) {
  TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP(IdentifierLayoutParameter, IdentifierLayoutParameter, Unique,
                                       IdentifierLayoutParameter);
}

void CompiledTransformer::OnLayout(const std::unique_ptr<raw::Layout>& el) {
  switch (el->kind) {
    case raw::Layout::kBits: {
      TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP(Layout, BitsDeclaration, Versioned, Bits);
      break;
    }
    case raw::Layout::kEnum: {
      TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP(Layout, EnumDeclaration, Versioned, Enum);
      break;
    }
    case raw::Layout::kStruct: {
      TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP(Layout, StructDeclaration, Versioned, Struct);
      break;
    }
    case raw::Layout::kTable: {
      TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP(Layout, TableDeclaration, Versioned, Table);
      break;
    }
    case raw::Layout::kUnion: {
      TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP(Layout, UnionDeclaration, Versioned, Union);
      break;
    }
    case raw::Layout::kOverlay: {
      TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP(Layout, OverlayDeclaration, Versioned, Overlay);
      break;
    }
  }
}

void CompiledTransformer::OnLayoutParameterList(
    const std::unique_ptr<raw::LayoutParameterList>& el) {
  TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP(LayoutParameterList, LayoutParameterList, Unique,
                                       LayoutParameterList);
}

void CompiledTransformer::OnLiteralLayoutParameter(
    const std::unique_ptr<raw::LiteralLayoutParameter>& el) {
  TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP(LiteralLayoutParameter, LiteralLayoutParameter, Unique,
                                       LiteralLayoutParameter);
  static_cast<void>(uptr.release());
}

void CompiledTransformer::OnNumericLiteral(raw::NumericLiteral& el) {
  TRANSFORM_REFERENCE_WITH_SOURCE_MAP(NumericLiteral, LiteralConstant);
}

void CompiledTransformer::OnOrdinaledLayoutMember(
    const std::unique_ptr<raw::OrdinaledLayoutMember>& el) {
  switch (el->layout_kind) {
    case raw::Layout::kTable: {
      TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP(OrdinaledLayoutMember, TableMember, Versioned,
                                           Table::Member);
      break;
    }
    case raw::Layout::kUnion: {
      TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP(OrdinaledLayoutMember, UnionMember, Versioned,
                                           Union::Member);
      break;
    }
    default:
      ZX_PANIC("should be unreachable");
  }
}

void CompiledTransformer::OnProtocolCompose(const std::unique_ptr<raw::ProtocolCompose>& el) {
  TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP(ProtocolCompose, ProtocolCompose, Versioned,
                                       Protocol::ComposedProtocol);
}

void CompiledTransformer::OnProtocolDeclaration(
    const std::unique_ptr<raw::ProtocolDeclaration>& el) {
  TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP(ProtocolDeclaration, ProtocolDeclaration, Versioned,
                                       Protocol);
}

void CompiledTransformer::OnProtocolMethod(const std::unique_ptr<raw::ProtocolMethod>& el) {
  TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP(ProtocolMethod, ProtocolMethod, Versioned, Protocol::Method);
}

void CompiledTransformer::OnResourceDeclaration(
    const std::unique_ptr<raw::ResourceDeclaration>& el) {
  TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP(ResourceDeclaration, ResourceDeclaration, Versioned,
                                       Resource);
}

void CompiledTransformer::OnResourceProperty(const std::unique_ptr<raw::ResourceProperty>& el) {
  TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP(ResourceProperty, ResourceProperty, Versioned,
                                       Resource::Property);
}

void CompiledTransformer::OnServiceDeclaration(const std::unique_ptr<raw::ServiceDeclaration>& el) {
  TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP(ServiceDeclaration, ServiceDeclaration, Versioned, Service);
}

void CompiledTransformer::OnServiceMember(const std::unique_ptr<raw::ServiceMember>& el) {
  TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP(ServiceMember, ServiceMember, Versioned, Service::Member);
}

void CompiledTransformer::OnStringLiteral(raw::StringLiteral& el) {
  TRANSFORM_REFERENCE_WITH_SOURCE_MAP(StringLiteral, LiteralConstant);
}

void CompiledTransformer::OnStructLayoutMember(const std::unique_ptr<raw::StructLayoutMember>& el) {
  TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP(StructLayoutMember, StructMember, Versioned, Struct::Member);
}

void CompiledTransformer::OnTypeConstraints(const std::unique_ptr<raw::TypeConstraints>& el) {
  TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP(TypeConstraints, TypeConstraints, Unique, TypeConstraints);
}

void CompiledTransformer::OnTypeConstructor(const std::unique_ptr<raw::TypeConstructor>& el) {
  TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP(TypeConstructor, TypeConstructor, Unique, TypeConstructor);
}

void CompiledTransformer::OnTypeLayoutParameter(
    const std::unique_ptr<raw::TypeLayoutParameter>& el) {
  TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP(TypeLayoutParameter, TypeLayoutParameter, Unique,
                                       TypeLayoutParameter);
}

void CompiledTransformer::OnValueLayoutMember(const std::unique_ptr<raw::ValueLayoutMember>& el) {
  switch (el->layout_kind) {
    case raw::Layout::kBits: {
      TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP(ValueLayoutMember, BitsMember, Versioned, Bits::Member);
      break;
    }
    case raw::Layout::kEnum: {
      TRANSFORM_UNIQUE_PTR_WITH_SOURCE_MAP(ValueLayoutMember, EnumMember, Versioned, Enum::Member);
      break;
    }
    default:
      ZX_PANIC("should be unreachable");
  }
}

}  // namespace fidl::fix
