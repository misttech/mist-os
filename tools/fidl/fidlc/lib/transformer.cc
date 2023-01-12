// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "tools/fidl/fidlc/include/fidl/transformer.h"

#include <lib/fit/function.h>
#include <zircon/assert.h>

#include "tools/fidl/fidlc/include/fidl/tree_visitor.h"

namespace fidl::fix {

namespace internal {

std::unique_ptr<MutableElementMap> MutableElementMapGenerator::Produce(
    const std::unique_ptr<raw::File>& raw_ast) {
  raw::DeclarationOrderTreeVisitor::OnFile(raw_ast);
  return std::move(map_);
}

void MutableElementMapGenerator::OnSourceElementStart(const raw::SourceElement& element) {
  auto mutable_element = const_cast<raw::SourceElement*>(&element);
  map_->insert({mutable_element->source_signature(), mutable_element});
  raw::DeclarationOrderTreeVisitor::OnSourceElementStart(element);
}

void TokenPointerResolver::OnToken(const Token* token) {
  // TODO(fxbug.dev/114357): Searching the entire |TokenSlice| for each |Token|, every time we make
  // a change, has N^2 complexity, where N is the number of |SourceElement|s we visit. This is a
  // good place to look for future optimizations.

  // Search the entire |TokenSlice| for this entry.
  auto found = std::find_if(token_slice_->begin(), token_slice_->end(),
                            [&](Token* entry) { return entry->span() == token->span(); });
  if (found != token_slice_->end()) {
    Token* token_in_ptr_list = *found;
    Token* token_in_raw_ast = const_cast<Token*>(token);
    *token_in_raw_ast = *token_in_ptr_list;
    *found = token_in_raw_ast;
  }
}

void Transformer::AddError(const std::string& msg) {
  errors_.push_back(Error{
      .step = step_,
      .msg = msg,
  });
}

bool Transformer::BuildTransformStates() {
  ZX_ASSERT(step_ == Step::kPreparing);

  for (const auto* source_file : source_files_) {
    std::unique_ptr<raw::File> immutable_raw_ast =
        ParseSource(source_file, experimental_flags_, reporter());
    if (immutable_raw_ast == nullptr) {
      AddError("could not parse input file " + std::string(source_file->filename()));
      continue;
    }

    std::unique_ptr<raw::File> mutable_raw_ast =
        ParseSource(source_file, experimental_flags_, reporter());
    ZX_ASSERT_MSG(mutable_raw_ast != nullptr,
                  "should be impossible for second parse of same source to fail");

    // We have two identical raw ASTs, derived from the same |SourceFile| in memory. We'll now walk
    // the second one, turning it into a map that we may access from the first as we visit each of
    // its nodes.
    std::unique_ptr<MutableElementMap> maybe_mutable_element_map =
        MutableElementMapGenerator().Produce(mutable_raw_ast);
    ZX_ASSERT_MSG(maybe_mutable_element_map != nullptr, "could not map input file");

    transform_states_.emplace_back(source_file, std::move(immutable_raw_ast),
                                   std::move(mutable_raw_ast),
                                   std::move(maybe_mutable_element_map));
  }

  if (HasErrors()) {
    return false;
  }
  return true;
}

std::optional<std::vector<std::string>> Transformer::Format() {
  ZX_ASSERT(step_ == Step::kTransformed);
  NextStep();

  std::vector<std::string> out;
  auto formatter = fmt::NewFormatter(100, reporter_);

  for (auto& transformed : transform_states_) {
    const SourceFile* original_source_file = transformed.source_file;
    raw::TokenPointerList token_ptr_list;
    for (const auto& token_ptr : *transformed.mutable_token_ptr_list) {
      token_ptr_list.push_back(token_ptr);
    }

    // TODO(fxbug.dev/114357): Currently, breakages here will be very opaque. We should improve this
    // to report more useful errors when formatting fails.
    std::optional<std::string> pretty_printed =
        formatter.Format(std::move(transformed.mutable_raw_ast), token_ptr_list,
                         original_source_file->data().size());
    if (!pretty_printed.has_value()) {
      AddError("Failed to pretty print " + (std::string(original_source_file->filename())) + "\n");
      continue;
    }

    // Format a second time, this time using the pretty printed source. We use this step to verify
    // that pretty-printing produced a valid FIDL file.
    std::optional<std::string> formatted = formatter.Format(
        SourceFile(std::string(original_source_file->filename()), pretty_printed.value()),
        experimental_flags_);
    if (!formatted.has_value()) {
      AddError("Failed to re-parse and format " + std::string(original_source_file->filename()) +
               "\n");
      continue;
    }

    out.push_back(formatted.value());
    index_++;
  }

  if (HasErrors()) {
    return std::nullopt;
  }
  NextStep();
  return out;
}

raw::SourceElement* Transformer::GetAsMutable(const raw::SourceElement* el) {
  // Get the correct mutable mirror element.
  auto mut = CurrentlyTransforming().mutable_element_map->find(el->source_signature());
  ZX_ASSERT_MSG(mut != CurrentlyTransforming().mutable_element_map->end(),
                "mutable copy of raw AST element not found");
  return mut->second;
}

TokenSlice Transformer::GetTokenSlice(Token from, Token until) {
  MutableTokenPointerList& list = *CurrentlyTransforming().mutable_token_ptr_list;
  std::optional<TokenIterator> maybe_begin = FindIn(list.begin(), list.end(), from);
  // TODO(fxbug.dev/114357): Here and below, should probably do something more recoverable than
  // asserting in the future.
  ZX_ASSERT(maybe_begin.has_value());

  TokenIterator begin = maybe_begin.value();
  std::optional<TokenIterator> maybe_end = FindIn(begin, list.end(), until);
  ZX_ASSERT(maybe_end.has_value());

  TokenIterator end = maybe_end.value() + 1;
  ZX_ASSERT(end <= list.end());

  current_slice_ = begin;
  return TokenSlice(data_stash_, token_stash_, list, begin, end);
}

TokenPointerResolver Transformer::GetTokenPointerResolver(TokenSlice* token_slice) {
  return TokenPointerResolver(token_slice);
}

void Transformer::NextStep() {
  switch (step_) {
    case Step::kNew: {
      step_ = Step::kPreparing;
      return;
    }
    case Step::kPreparing: {
      step_ = Step::kPrepared;
      return;
    }
    case Step::kPrepared: {
      step_ = Step::kTransforming;
      return;
    }
    case Step::kTransforming: {
      step_ = Step::kTransformed;
      return;
    }
    case Step::kTransformed: {
      step_ = Step::kFormatting;
      return;
    }
    case Step::kFormatting: {
      step_ = Step::kSuccess;
      return;
    }
    case Step::kSuccess:
    case Step::kFailure:
      ZX_PANIC("No next step after success/failure!");
  }
}

std::unique_ptr<raw::File> Transformer::ParseSource(
    const SourceFile* source_file, const fidl::ExperimentalFlags& experimental_flags,
    Reporter* reporter) {
  fidl::Lexer lexer(*source_file, reporter);
  Parser parser(&lexer, reporter, experimental_flags);
  std::unique_ptr<raw::File> ast = parser.Parse();
  if (!parser.Success()) {
    return nullptr;
  }

  return ast;
}

bool Transformer::Transform() {
  ZX_ASSERT(step_ == Step::kPrepared);
  NextStep();

  for (auto& transforming : transform_states_) {
    auto ptr_list_location = CurrentlyTransforming().mutable_token_ptr_list->begin();
    OnFile(transforming.immutable_raw_ast);

    // TODO(fxbug.dev/114357): as noted in the comment on |TokenIterator|, we currently pre-allocate
    // a large vector to try and avoid this, and use the following assert to error fast at runtime,
    // but better solutions are possible.
    ZX_ASSERT(ptr_list_location == CurrentlyTransforming().mutable_token_ptr_list->begin());

    index_++;
  }

  if (HasErrors()) {
    return false;
  }
  NextStep();
  return true;
}

}  // namespace internal

TokenIterator TokenSlice::AddTokenAfter(TokenIterator prev_it, const std::string& text,
                                        Token::Kind kind, Token::Subkind subkind) {
  return AddTokenBefore(prev_it + 1, text, kind, subkind);
}

TokenIterator TokenSlice::AddTokenBefore(TokenIterator next_it, const std::string& text,
                                         Token::Kind kind, Token::Subkind subkind) {
  // Make sure the token we are inserting before is after the |kStartOFile| token, but still in the
  // underlying vector. Also disallow inserting a token after the |kEndOfFile| token.
  ZX_ASSERT(next_it > underlying_.begin());
  ZX_ASSERT(next_it < underlying_.end());
  TokenIterator prev_it = next_it - 1;
  Token* prev_token = *prev_it;
  uint32_t prev_ordinal = prev_token->ordinal();
  uint32_t prev_sub_ordinal = prev_token->sub_ordinal();

  // Insert this token into storage, then add a pointer to that storage to this slice.
  data_stash_.push_back(std::make_unique<std::string>(text));
  Token* next_token_ptr = *next_it;
  std::string_view data = std::string_view(data_stash_.back()->c_str());
  auto new_span = SourceSpan(data, prev_token->span().source_file());
  auto leading_newlines = next_token_ptr->leading_newlines();
  next_token_ptr->set_leading_newlines(0);
  token_stash_.emplace_back(std::make_unique<SyntheticToken>(
      new_span, leading_newlines, kind, subkind, prev_ordinal, prev_sub_ordinal + 1));

  // TODO(fxbug.dev/114357): As noted in the comment on |TokenIterator|, the following operation has
  // the potential to cause a re-allocation of the underlying storage, thereby invalidating all
  // |TokenIterator|s (including |prev_it|, |next_it|, and |new_token_it| here, as well as any other
  // |TokenIterator|s minted before this method was called). We currently pre-allocate a large
  // vector to try and avoid this, and use the following assert to error fast at runtime, but better
  // solutions are possible.
  auto ptr_list_location = underlying_.begin();
  underlying_.insert(next_it, token_stash_.back().get());
  ZX_ASSERT(ptr_list_location == underlying_.begin());

  // The |new_token_it| points the position of the previous |next_it|, which now has to be
  // incremented to continue pointing at the same |Token*| entry as before.
  TokenIterator new_token_it = next_it;
  next_it++;
  end_++;

  // Walk forward, incrementing the |sub_ordinal| of every entry that has the same |ordinal| as the
  // |SyntheticToken| we just created. This will keep our |MutableTokenPointerList| properly
  // ordered.
  while ((*next_it)->ordinal() == prev_ordinal) {
    // A shared |ordinal| with a predecessor guarantees that this is a |SyntheticToken|.
    auto synthetic = static_cast<SyntheticToken*>(*next_it);
    synthetic->set_sub_ordinal(synthetic->sub_ordinal() + 1);
    prev_ordinal++;
    next_it++;
  }

  return new_token_it;
}

bool TokenSlice::DropSourceElement(raw::SourceElement* element) {
  std::optional<TokenIterator> maybe_start =
      SearchForward([&](const Token* entry) { return entry->span() == element->start().span(); });
  if (!maybe_start.has_value()) {
    return false;
  }
  TokenIterator start = maybe_start.value();

  std::optional<TokenIterator> maybe_end = SearchForward(
      start, [&](const Token* entry) { return entry->span() == element->end().span(); });
  if (!maybe_end.has_value()) {
    return false;
  }
  TokenIterator end = maybe_end.value();

  // Starting from the end and walking backwards, drop each element one at a time.
  TokenIterator curr = end;
  while (curr >= start) {
    DropToken(curr);
    curr--;
  }

  return true;
}

void TokenSlice::DropToken(TokenIterator pos) {
  // Looking one back is always safe, because we are guaranteed to at least have a
  // |Token::Kind::kStartOfFile|. Looking one ahead is always safe, because we are guaranteed to at
  // least have a |Token::Kind::kEndOfFile|.
  ZX_ASSERT(pos > underlying_.begin());
  ZX_ASSERT(pos < underlying_.end() - 1);
  TokenIterator prev = pos - 1;
  TokenIterator next = pos + 1;

  // A non-zero |prev_sub_ordinal| means we're deleting a |SyntheticToken|, while zero means that
  // we're deleting a sourced one. These will need to be handled differently when adjusting
  // |sub_ordinals| below.
  Token* erasing = *pos;
  ZX_ASSERT(erasing->kind() != Token::kStartOfFile && erasing->kind() != Token::kEndOfFile);
  bool erasing_synthetic = erasing->is_synthetic();
  uint32_t prev_sub_ordinal = erasing_synthetic ? (*prev)->sub_ordinal() : 0;

  // Retain some useful information from this |Token|, then erase it from the |TokenSlice|.
  uint32_t erasing_ordinal = erasing->ordinal();
  (*next)->set_leading_newlines((*prev)->leading_newlines());
  underlying_.erase(pos);
  ZX_ASSERT(--end_ >= begin_);

  // Walk forward, adjusting the |sub_ordinal| of every entry that has the same |ordinal| as the
  // |Token| we just just erased. This will keep our |MutableTokenPointerList| properly ordered.
  while ((*pos)->ordinal() == erasing_ordinal) {
    // A shared |ordinal| with a predecessor guarantees that this is a |SyntheticToken|.
    auto synthetic = static_cast<SyntheticToken*>(*pos);
    if (!erasing_synthetic) {
      // This branch means that the deleted |Token| was an original, sourced |Token|. Any
      // |SyntheticToken|s that may have relied on its ordinal must be "appended" to its
      // predecessor. For example: suppose we have a |SyntheticToken| with an
      // |ordinal|:|sub_ordinal| pair of 46:2, followed by an original |Token| at 47:0, followed by
      // another |SyntheticToken| at 47:1.  If we delete the original |Token| at 47:0, all of its
      // chained |SyntheticToken|s are "re-assigned" to its predecessor, so 47:1 must become 46:3
      // instead. The calculation below accomplishes this.
      synthetic->set_ordinal(erasing_ordinal - 1);
      synthetic->set_sub_ordinal(++prev_sub_ordinal);
    } else {
      // The erased |Token| was itself a |SyntheticToken|. We can get away with just decrementing
      // every subsequent |SyntheticToken| assigned to this |ordinal| by 1.
      synthetic->set_sub_ordinal(synthetic->sub_ordinal() - 1);
    }
    ZX_ASSERT(++pos <= underlying_.end());
  }

  // If we removed a sourced |Token|, we'll need to decrement every remaining token in the entire
  // |MutableTokenPointerList| (not just this |TokenSlice|!) by 1.
  if (!erasing_synthetic) {
    while (pos < underlying_.end()) {
      (*pos)->set_ordinal((*pos)->ordinal() - 1);
      pos++;
    }
  }
}

std::optional<TokenIterator> TokenSlice::SearchBackward(TokenIterator from, TokenMatcher matcher) {
  TokenIterator curr = from;
  while (curr >= begin()) {
    if (matcher(*curr)) {
      return std::make_optional<TokenIterator>(curr);
    }
    curr--;
  }
  return std::nullopt;
}

std::optional<TokenIterator> TokenSlice::SearchBackward(TokenMatcher matcher) {
  return SearchBackward(end(), std::move(matcher));
}

std::optional<TokenIterator> TokenSlice::SearchForward(TokenIterator from, TokenMatcher matcher) {
  TokenIterator matched = std::find_if(from, end(), std::move(matcher));
  if (matched == end()) {
    return std::nullopt;
  }
  return std::make_optional<TokenIterator>(matched);
}

std::optional<TokenIterator> TokenSlice::SearchForward(TokenMatcher matcher) {
  return SearchForward(begin(), std::move(matcher));
}

bool TokenSlice::UpdateTokenPointer(Token* raw_ast_token_ptr, TokenIterator new_token_it) {
  Token* new_token_ptr = *new_token_it;

  // Find the pointer to the |raw_ast_token_ptr|'s memory in this |TokenSlice|. We want to make sure
  // that this still points to the correct |Token| (that |Token| hasn't been removed yet, after
  // all), but not into the raw AST, since that |Token| is being updated to a new value.
  std::optional<TokenIterator> maybe_found_it =
      SearchForward([&](const Token* entry) { return entry->span() == raw_ast_token_ptr->span(); });
  if (!maybe_found_it.has_value()) {
    return false;
  }

  // Take the value of the raw AST |Token| pointed to by |raw_ast_token_ptr|, and add it to the
  // |token_stash_|.
  token_stash_.emplace_back(std::make_unique<Token>(*raw_ast_token_ptr));

  // The pointer in the |TokenSlice| that previously pointed at this raw AST node is updated to
  // point to the newly created |Token| entry. The ensures that the |Token| is still retained in the
  // correct position in the |MutableTokenPointerList|, even if it is no longer represented in the
  // raw AST itself. To keep this |Token| around, we need a new home for it where we can point to,
  // so we append it to the |token_stash_| instead.
  TokenIterator found_it = maybe_found_it.value();
  *found_it = token_stash_.back().get();

  // The value of the memory pointed to by |raw_ast_token_ptr| is updated to hold the new value
  // represented by |new_token_it|, thereby inserting that |Token| into the raw AST. The entry in
  // the |MutableTokenPointerList| is updated to point into the raw AST, leaving its previously
  // pointed to |token_stash_| entry an orphan.
  *raw_ast_token_ptr = *new_token_ptr;
  *new_token_it = raw_ast_token_ptr;

  return true;
}

}  // namespace fidl::fix
