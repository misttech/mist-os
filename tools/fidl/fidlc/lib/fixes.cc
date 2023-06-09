// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "tools/fidl/fidlc/include/fidl/fixes.h"

#include <stdio.h>
#include <unistd.h>

#include <optional>

#include "lib/fit/result.h"
#include "tools/fidl/fidlc/include/fidl/diagnostic_types.h"
#include "tools/fidl/fidlc/include/fidl/formatter.h"
#include "tools/fidl/fidlc/include/fidl/lexer.h"
#include "tools/fidl/fidlc/include/fidl/parser.h"
#include "tools/fidl/fidlc/include/fidl/reporter.h"
#include "tools/fidl/fidlc/include/fidl/transformer.h"
#include "tools/fidl/fidlc/include/fidl/versioning_types.h"

namespace fidl::fix {

Status Fix::ValidateFlags() {
  bool ok = true;
  fixable_.required_flags.ForEach(
      [&, this](const std::string_view, ExperimentalFlags::Flag flag, bool is_required) {
        if (is_required && !this->experimental_flags_.IsFlagEnabled(flag)) {
          ok = false;
        }
      });

  return ok ? Status::kOk : Status::kErrorOther;
};

std::vector<const SourceFile*> Fix::GetSourceFiles() {
  std::vector<const SourceFile*> source_file_ptrs;
  for (const auto& source_file : library_->sources()) {
    source_file_ptrs.emplace_back(source_file.get());
  }

  return source_file_ptrs;
};

template <typename T>
TransformResult Fix::Execute(std::unique_ptr<T> transformer,
                             const std::vector<const SourceFile*>& source_files,
                             Reporter* reporter) {
  if (!transformer->Prepare()) {
    return fit::error(Failure{.status = Status::kErrorPreFix, .errors = transformer->GetErrors()});
  }

  if (!transformer->Transform()) {
    return fit::error(
        Failure{.status = Status::kErrorDuringFix, .errors = transformer->GetErrors()});
  }

  std::optional<std::vector<std::string>> formatted = transformer->Format();
  if (!formatted.has_value()) {
    return fit::error(Failure{.status = Status::kErrorPostFix, .errors = transformer->GetErrors()});
  }

  OutputMap out;
  std::vector<std::string> results = formatted.value();
  ZX_ASSERT(results.size() == source_files.size());
  for (size_t i = 0; i < source_files.size(); i++) {
    out.insert({std::string(source_files[i]->filename()), std::move(results[i])});
  }

  return fit::ok(out);
};

TransformResult ParsedFix::Transform(Reporter* reporter) {
  const std::vector<const SourceFile*> source_files = GetSourceFiles();
  return Execute(GetParsedTransformer(source_files, experimental_flags_, reporter), source_files,
                 reporter);
};

// Transformer that performs no transformation at all. Intended as both a placeholder (when there
// are no active transformations in the codebase) and as an example.
class NoopTransformer final : public fix::ParsedTransformer {
 public:
  NoopTransformer(const std::vector<const SourceFile*> source_files,
                  const fidl::ExperimentalFlags& experimental_flags, Reporter* reporter)
      : fix::ParsedTransformer(source_files, experimental_flags, reporter) {}
};

std::unique_ptr<ParsedTransformer> NoopParsedFix::GetParsedTransformer(
    const std::vector<const SourceFile*>& source_files,
    const fidl::ExperimentalFlags& experimental_flags, Reporter* reporter) {
  return std::make_unique<NoopTransformer>(source_files, experimental_flags_, reporter);
}

TransformResult CompiledFix::Transform(Reporter* reporter) {
  const std::vector<const SourceFile*> library_source_files = GetSourceFiles();
  std::vector<std::vector<const SourceFile*>> dependency_source_files;
  for (const auto& dependency : dependencies_) {
    dependency_source_files.emplace_back();
    std::vector<const SourceFile*>& source_file_ptrs = dependency_source_files.back();
    for (const auto& source_file : dependency->sources()) {
      source_file_ptrs.emplace_back(source_file.get());
    }
  }

  return Execute(GetCompiledTransformer(library_source_files, dependency_source_files,
                                        version_selection_, experimental_flags_, reporter),
                 library_source_files, reporter);
};

// Transformer to add `closed` before `protocol` definitions with no leading modifiers.
class ProtocolModifiersTransformer final : public fix::ParsedTransformer {
 public:
  ProtocolModifiersTransformer(const std::vector<const SourceFile*> source_files,
                               const fidl::ExperimentalFlags& experimental_flags,
                               Reporter* reporter)
      : fix::ParsedTransformer(source_files, experimental_flags, reporter) {}

 private:
  void WhenProtocolDeclaration(raw::ProtocolDeclaration* el,
                               fix::TokenSlice& token_slice) override {
    if (el->modifiers != nullptr && el->modifiers->maybe_openness.has_value()) {
      // Already has openness modifier, nothing to do.
      return;
    }

    // Find the token representing the protocol name.
    std::optional<fix::TokenIterator> maybe_protocol_identifier_token_it =
        token_slice.SearchForward(
            [&](const Token* entry) { return entry->span() == el->identifier->span(); });
    if (!maybe_protocol_identifier_token_it.has_value()) {
      return AddError("Unable to find protocol identifier token - raw AST corrupted.");
    }

    // Walk backwards from the node before the protocol name, searching for the first instance of
    // the `protocol`. This is the safest way to find the actual `protocol` keyword, even in the
    // face of other "protocol" identifiers in the declaration (ex: an `@protocol` attribute).
    fix::TokenIterator protocol_identifier_token_it = maybe_protocol_identifier_token_it.value();
    std::optional<fix::TokenIterator> maybe_protocol_token_it =
        token_slice.SearchBackward(protocol_identifier_token_it - 1, [](const Token* entry) {
          return entry->kind() == Token::Kind::kIdentifier &&
                 entry->subkind() == Token::Subkind::kProtocol;
        });
    if (!maybe_protocol_token_it.has_value()) {
      return AddError("Unable to find `protocol` token - raw AST corrupted.");
    }

    // Create the new token we'll be inserting.
    fix::TokenIterator protocol_token_it = maybe_protocol_token_it.value();
    fix::TokenIterator new_token_it = token_slice.AddTokenBefore(
        protocol_token_it, "closed", Token::Kind::kIdentifier, Token::Subkind::kClosed);

    // No other modifiers are possible on a protocol declaration, so it is safe to create the
    // new |raw::Modifiers| node from scratch, as no existing information will be lost.
    auto modifier = std::make_optional<raw::Modifier<types::Openness>>(types::Openness::kClosed,
                                                                       **new_token_it);
    el->modifiers =
        std::make_unique<raw::Modifiers>(NewTokenChain(new_token_it, new_token_it), modifier);

    // Only update the start pointer if it was previously pointed at the `protocol` token.
    if (el->start().kind() == Token::Kind::kIdentifier &&
        el->start().subkind() == Token::Subkind::kProtocol) {
      token_slice.UpdateTokenPointer(&el->start(), new_token_it);
    }
  }

  void WhenProtocolMethod(raw::ProtocolMethod* el, fix::TokenSlice& token_slice) override {
    if (el->modifiers != nullptr && el->modifiers->maybe_strictness.has_value()) {
      // Already has strictness modifier, nothing to do.
      return;
    }

    // The |start| node of this |SourceElement| may not necessarily be the method name (for
    // example, if the node starts with an attribute).
    std::optional<fix::TokenIterator> maybe_method_start_token_it =
        token_slice.SearchForward([&](const Token* entry) {
          if (el->maybe_request == nullptr) {
            // For events, find the arrow.
            return entry->kind() == Token::Kind::kArrow;
          } else {
            // For all other methods, the |Token| of interest is always the method identifier.
            return entry->span() == el->identifier->span();
          }
        });
    if (!maybe_method_start_token_it.has_value()) {
      return AddError("Unable to find method name token - raw AST corrupted.");
    }

    // Create the new token we'll be inserting.
    fix::TokenIterator method_start_token_it = maybe_method_start_token_it.value();
    fix::TokenIterator new_token_it = token_slice.AddTokenBefore(
        method_start_token_it, "strict", Token::Kind::kIdentifier, Token::Subkind::kStrict);

    // No other modifiers are possible on a method declaration, so it is safe to create the
    // new |raw::Modifiers| node from scratch, as no existing information will be lost.
    auto modifier = std::make_optional<raw::Modifier<types::Strictness>>(types::Strictness::kStrict,
                                                                         **new_token_it);
    el->modifiers =
        std::make_unique<raw::Modifiers>(NewTokenChain(new_token_it, new_token_it), modifier);

    // Only update the start pointer if there is no leading attribute already occupying that slot.
    if (el->attributes == nullptr || el->attributes->attributes.empty()) {
      token_slice.UpdateTokenPointer(&el->start(), new_token_it);
    }
  }
};

std::unique_ptr<ParsedTransformer> ProtocolModifierFix::GetParsedTransformer(
    const std::vector<const SourceFile*>& source_files,
    const fidl::ExperimentalFlags& experimental_flags, Reporter* reporter) {
  return std::make_unique<ProtocolModifiersTransformer>(source_files, experimental_flags_,
                                                        reporter);
}

// Transformer to remove empty struct payloads from error-bearing method responses.
class EmptyStructResponseTransformer final : public fix::CompiledTransformer {
 public:
  EmptyStructResponseTransformer(
      const std::vector<const SourceFile*> library_source_files,
      const std::vector<std::vector<const SourceFile*>>& dependencies_source_files,
      const fidl::ExperimentalFlags& experimental_flags,
      const fidl::VersionSelection* version_selection, Reporter* reporter)
      : fix::CompiledTransformer(library_source_files, dependencies_source_files, version_selection,
                                 experimental_flags, reporter) {}

 private:
  void WhenProtocolMethod(raw::ProtocolMethod* el, TokenSlice& token_slice,
                          const VersionedEntry<flat::Protocol::Method>* entry) override {
    // If the latest variant is not an empty payload struct in a result union, there is nothing to
    // do.
    const flat::Protocol::Method* compiled = entry->Newest();
    std::optional<const flat::Struct*> latest_success_variant =
        GetEmptyStructForResultSuccessVariant(*compiled);
    if (!latest_success_variant.has_value()) {
      return;
    }

    // If this is an anonymous, compiler-generated name, we are looking at an already correct
    // instance, so there is nothing to do.
    auto* anonymous = latest_success_variant.value()->name.as_anonymous();
    if (anonymous && anonymous->provenance == flat::Name::Provenance::kCompilerGenerated) {
      return;
    }

    // Ensure that we avoid situations where this is a struct that is currently empty, but was
    // previously membered. While this is an obvious ABI breakage and should not occur, it is still
    // technically possible, so we should error cleanly.
    bool previously_had_members = false;
    entry->ForEach([&](const VersionRange& range, const flat::Protocol::Method& method) {
      if (GetEmptyStructForResultSuccessVariant(method) == std::nullopt) {
        // TODO(fxbug.dev/118371): We could probably create something similar to |Reporter|, or even
        // use that class directly, to generalize transformation error message construction.
        AddError("Unexpectedly unfixable result payload struct at " + method.name.position_str() +
                 ": '" + method.owning_protocol->GetName() + "." + std::string(method.name.data()) +
                 "' did not have an empty struct payload " + fidl::internal::Display(range));
        previously_had_members = true;
      }
    });
    if (previously_had_members) {
      return;
    }

    // Delete the offending entry-> Because the `struct`'s type constructor could not have been
    // the |start()| or |end()| node of any of its parent elements, we don't need to do any
    // |UpdateTokenPointer| calls here.
    token_slice.DropSourceElement(el->maybe_response->type_ctor.get());
    el->maybe_response->type_ctor = nullptr;
  }

  // We only want to convert two-way methods that use a result union, with an empty payload struct,
  // for the response. If any of those things are untrue about this method, just ignore it, as
  // there's nothing to do.
  std::optional<const flat::Struct*> GetEmptyStructForResultSuccessVariant(
      const flat::Protocol::Method& compiled) {
    if (!compiled.HasResultUnion()) {
      return std::nullopt;
    }

    ZX_ASSERT(compiled.maybe_response->type->kind == flat::Type::Kind::kIdentifier);
    auto wrapper_id = static_cast<const flat::IdentifierType*>(compiled.maybe_response->type);
    ZX_ASSERT(wrapper_id->type_decl->kind == flat::Decl::Kind::kStruct);
    auto wrapper_struct = static_cast<const flat::Struct*>(wrapper_id->type_decl);
    ZX_ASSERT(!wrapper_struct->members.empty());

    const auto* result_union_id =
        static_cast<const flat::IdentifierType*>(wrapper_struct->members[0].type_ctor->type);
    ZX_ASSERT(result_union_id->type_decl->kind == flat::Decl::Kind::kUnion);
    const auto* result_union = static_cast<const flat::Union*>(result_union_id->type_decl);
    ZX_ASSERT(!result_union->members.empty());
    ZX_ASSERT(result_union->members[0].maybe_used);

    const auto* success_variant_type = result_union->members[0].maybe_used->type_ctor->type;
    if (!success_variant_type) {
      return std::nullopt;
    }

    ZX_ASSERT(success_variant_type->kind == flat::Type::Kind::kIdentifier);
    auto success_variant_id = static_cast<const flat::IdentifierType*>(success_variant_type);
    if (success_variant_id->type_decl->kind != flat::Decl::Kind::kStruct) {
      return std::nullopt;
    }

    auto success_variant_struct = static_cast<const flat::Struct*>(success_variant_id->type_decl);
    if (!success_variant_struct->members.empty()) {
      return std::nullopt;
    }

    return std::make_optional(success_variant_struct);
  }
};

std::unique_ptr<CompiledTransformer> EmptyStructResponseFix::GetCompiledTransformer(
    const std::vector<const SourceFile*>& library_source_files,
    const std::vector<std::vector<const SourceFile*>>& dependencies_source_files,
    const fidl::VersionSelection* version_selection,
    const fidl::ExperimentalFlags& experimental_flags, Reporter* reporter) {
  return std::make_unique<EmptyStructResponseTransformer>(
      library_source_files, dependencies_source_files, experimental_flags, version_selection,
      reporter);
}

}  // namespace fidl::fix
