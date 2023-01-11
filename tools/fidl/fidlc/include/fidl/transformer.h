// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef TOOLS_FIDL_FIDLC_INCLUDE_FIDL_TRANSFORMER_H_
#define TOOLS_FIDL_FIDLC_INCLUDE_FIDL_TRANSFORMER_H_

#include <lib/fit/function.h>
#include <zircon/assert.h>

#include <algorithm>
#include <set>
#include <tuple>
#include <vector>

#include "tools/fidl/fidlc/include/fidl/experimental_flags.h"
#include "tools/fidl/fidlc/include/fidl/flat/compiler.h"
#include "tools/fidl/fidlc/include/fidl/formatter.h"
#include "tools/fidl/fidlc/include/fidl/lexer.h"
#include "tools/fidl/fidlc/include/fidl/ordinals.h"
#include "tools/fidl/fidlc/include/fidl/parser.h"
#include "tools/fidl/fidlc/include/fidl/raw_ast.h"
#include "tools/fidl/fidlc/include/fidl/reporter.h"
#include "tools/fidl/fidlc/include/fidl/source_map.h"
#include "tools/fidl/fidlc/include/fidl/token.h"
#include "tools/fidl/fidlc/include/fidl/token_list.h"
#include "tools/fidl/fidlc/include/fidl/tree_visitor.h"
#include "tools/fidl/fidlc/include/fidl/versioning_types.h"
#include "tools/fidl/fidlc/include/fidl/virtual_source_file.h"

// A macro containing the logic of calling any particular user-supplied |When*| function, which
// should be the same regardless of the particular |NodeKind| being visited.
//
// What's happening here:
//   1. Visit all children first (ie, do post-order traversal of the raw AST).
//   2. Search the |MutableElementMap| to find the copy of the node we're currently visiting int the
//      mutable raw AST.
//   3. Get a slice into the |Token|s covered by that raw AST node, inclusive of that node's
//      |start()| and |end()| tokens.
//   4. Create a |TokenPointerResolver|, so that we can do a bit of automated repair after the
//      user-supplied |When*| method has completed.
//   5. Call the user-supplied |When*| logic.
//   6. Cleanup using the |TokenPointerResolver|.
//
#define VISIT_THEN_TRANSFORM_UNIQUE_PTR(RAW_AST_NODE, WHEN)              \
  DeclarationOrderTreeVisitor::On##RAW_AST_NODE(el);                     \
  auto mut = static_cast<raw::RAW_AST_NODE*>(GetAsMutable(el.get()));    \
  TokenSlice token_slice = GetTokenSlice(el->start(), el->end());        \
  TokenPointerResolver resolver = GetTokenPointerResolver(&token_slice); \
  When##WHEN(mut, token_slice);                                          \
  std::unique_ptr<raw::RAW_AST_NODE> uptr(mut);                          \
  resolver.On##RAW_AST_NODE(uptr);                                       \
  static_cast<void>(uptr.release());

// A few visitor methods defined by |TreeVisitor| supply const references, rather than const
// |unique_ptr<...>| references, as their sole parameter. This macro does the same thing as
// |VISIT_THEN_TRANSFORM_UNIQUE_PTR| for such cases.
#define VISIT_THEN_TRANSFORM_REFERENCE(RAW_AST_NODE)                     \
  DeclarationOrderTreeVisitor::On##RAW_AST_NODE(el);                     \
  auto mut = static_cast<raw::RAW_AST_NODE*>(GetAsMutable(&el));         \
  TokenSlice token_slice = GetTokenSlice(el.start(), el.end());          \
  TokenPointerResolver resolver = GetTokenPointerResolver(&token_slice); \
  When##RAW_AST_NODE(mut, token_slice);                                  \
  resolver.On##RAW_AST_NODE(*mut);

namespace fidl::fix {

// Very similar to the a |TokenPointerList|, this represents a list of |Token|s parsed from a file.
// The |Token|s are represented as pointers into one of three locations: the raw AST for |Token|s
// that serve as |start()| or |end()| nodes there, the raw AST's "side" token stash for all
// "sourced" (aka from the original file buffer) |Token|s that do not, and a third stash stored in
// some |Transformer| containing the backing buffer for all strings that back newly created
// |SyntheticToken|s.
using MutableTokenPointerList = std::vector<Token*>;

// A view into a |TokenSlice|. Care must be taken when using as, like any iterator, previous values
// are invalidated as the underlying |TokenSlice| is mutated.
//
// TODO(fxbug.dev/114357): This iterator currently does not survive resizing. This means that any
// |MutableTokenPointerList| expanding operation has the potential to invalidate all previously
// minted |TokenIterator|s, since the underlying memory they point to has been re-allocated. Our
// current "solution" to this problem is to simply pre-allocate a very large underlying token list
// (see the |Transformer| constructor - currently 2x the size of the original file) and asserting
// that the vector never grows beyond this size, but this is unideal. A better future solution would
// be to make |TokenIterator| a wrapper class that only tracks the offset into the underlying vector
// and has various overloads to "hide" this fact from users, thereby allowing any given instance to
// survive a reallocation of the underlying storage.
using TokenIterator = MutableTokenPointerList::iterator;

// Finds a specific |Token| of interest in a |MutableTokenPointerList|, given a specific start
// point. A typed convenience wrapper around |std::find|.
inline std::optional<TokenIterator> FindIn(TokenIterator begin, TokenIterator end, Token needle) {
  TokenIterator matched =
      std::find_if(begin, end, [&needle](const Token* entry) { return *entry == needle; });
  if (matched == end) {
    return std::nullopt;
  }
  return std::make_optional<TokenIterator>(matched);
}

// This is a helper function for making a new |raw::TokenChain| for insertion into a raw AST. It
// saves callers some unsavory double dereferences.
inline raw::TokenChain NewTokenChain(TokenIterator from, TokenIterator until) {
  return raw::TokenChain(**from, **until);
}

// A view into the |MutableTokenPointerList| underpinning some raw AST which supports easy mutation
// of the supplied range of |Token|s it contains.
//
// Successful transformation requires keeping two data structures (the raw abstract syntax tree, and
// the |MutableTokenPointerList| that represents it's in-order underlying tokens) in sync. The
// |TokenSlice| is an abstraction for making this process a bit smoother. Perform operations on the
// |TokenSlice| first, then use the resulting outputs to manipulate the raw AST.
//
// A |TokenSlice| supports three basic mutations (though some come in multiple flavors):
//
//   1. Insertion ("Add*" methods): Adding a new |Token| at some specified point in the list. The
//      |TokenSlice| inserts a new entry into the list of |Token|s, returning copies of any new
//      |Token|s it creates for safe insertion into the AST.
//   2. Removal ("Drop*" methods): |Token|s must be removed from the |TokenSlice| before they are
//      dropped from the raw AST it is paired with.
//   3. Reconciliation ("Update*" methods): It is necessary to ensure that all other references to a
//      |Token| are removed from the raw AST before we change it. These methods provide that
//      functionality. For example, if we were to add a `resource` modifier to prefix the `union {}`
//      |raw::TypeConstructor|, we would need to take care to update the
//      |raw::InlineLayoutReference| and |raw::Layout| it contains that also start with the (a copy
//      of) the same |Token| before doing so.
//
// Additionally, the |TokenSlice| provides some |Search*| methods for easily locating |Token|s of
// interest.
class TokenSlice {
 public:
  TokenSlice(std::vector<std::unique_ptr<std::string>>& data_stash,
             std::vector<std::unique_ptr<Token>>& token_stash, MutableTokenPointerList& underlying,
             TokenIterator begin, TokenIterator end)
      : data_stash_(data_stash),
        token_stash_(token_stash),
        underlying_(underlying),
        begin_(begin - underlying_.begin()),
        end_(end - underlying_.begin()) {}

  TokenIterator begin() { return underlying_.begin() + begin_; }
  TokenIterator end() { return underlying_.begin() + end_; }

  using TokenMatcher = std::function<bool(const Token*)>;

  // Walk the |TokenSlice| from beginning to end, looking for the first |Token| that matches on all
  // of the supplied search parameters. Its main benefit is that it absolves the user from keeping
  // tracking of the |end()| themselves, as it will change with each mutation.
  std::optional<TokenIterator> SearchForward(TokenIterator from, TokenMatcher matcher) {
    TokenIterator matched = std::find_if(from, end(), std::move(matcher));
    if (matched == end()) {
      return std::nullopt;
    }
    return std::make_optional<TokenIterator>(matched);
  }
  std::optional<TokenIterator> SearchForward(TokenMatcher matcher) {
    return SearchForward(begin(), std::move(matcher));
  }

  // Walk the |TokenSlice| from end to beginning, looking for the first |Token| that matches on all
  // of the supplied search parameters. Its main benefit is that it absolves the user from keeping
  // tracking of the |end()| themselves, as it will change with each mutation.
  std::optional<TokenIterator> SearchBackward(TokenIterator from, TokenMatcher matcher) {
    TokenIterator curr = from;
    while (curr >= begin()) {
      if (matcher(*curr)) {
        return std::make_optional<TokenIterator>(curr);
      }
      curr--;
    }
    return std::nullopt;
  }
  std::optional<TokenIterator> SearchBackward(TokenMatcher matcher) {
    return SearchBackward(end(), std::move(matcher));
  }

  // Remove a |TokenIterator| from the |TokenSlice|. If the token is not represented at all in the
  // raw AST, this operation alone is sufficient. However, if the |Token| is used as a |start()| or
  // |end()| node anywhere in the raw AST, those nodes must first have |UpdateTokenPointer| called
  // on them to give them new values before this |Token| can be dropped.
  //
  // It is important to note that a single |TokenIterator| may be represented by multiple
  // |start()|/|end()| tokens in the raw AST, and all of those need to be updated before this method
  // can be safely called!
  void DropToken(TokenIterator pos) {
    // Looking one back is always safe, because we are guaranteed to at least have a
    // |Token::Kind::kStartOfFile|. Looking one ahead is always safe, because we are guaranteed to
    // at least have a |Token::Kind::kEndOfFile|.
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
        // |ordinal|:|sub_ordinal| pair of 46:2, followed by an original |Token| at 47:0, followed
        // by another |SyntheticToken| at 47:1.  If we delete the original |Token| at 47:0, all of
        // its chained |SyntheticToken|s are "re-assigned" to its predecessor, so 47:1 must become
        // 46:3 instead. The calculation below accomplishes this.
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

  // A method that proves sugar for calling |DropToken| sequentially on every |Token| contained in
  // some |raw::SourceElement|. All of the rules from that method still apply to the |start()| and
  // |end()| tokens of the |raw::SourceElement| being dropped: if they are shared with any other
  // elements in the raw AST, those elements must be updated to use different |Token|s before this
  // drop occurs.
  bool DropSourceElement(raw::SourceElement* element) {
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

  // This method is called any time we want to take an existing token and place a copy of it
  // somewhere else in the raw AST. This can occur when we're adding new tokens (thereby causing the
  // |start()|/|end()| tokens of related raw AST nodes to shift), or when dropping them. Prior to
  // doing either of those actions, one should take care to call |UpdateTokenPointer| on the
  // relevant raw AST |Token| instances first.
  //
  // Consider the cae of a "pseudo" FIDL file, `a b c d e`, parsed into a single raw AST node. It
  // would look like this after parsing:
  //
  //     RAW AST NODE          SOURCE TOKEN STASH
  //   -----------------    -------------------------
  //   |start()| end() |    |   0   |   1   |   2   |
  //   -----------------    -------------------------
  //   |   a   |   e   |    |   b   |   c   |   d   |
  //       |       |          /       /       /
  //       |       |         /       /       /
  //       |       |        /       /       /
  //       |       |       /       /       /
  //       |       |      /       /       /
  //       |       |     /       /       /
  //       |        -----------------------
  //       |           /       /       /   |
  //       |          /       /       /    |
  //       |         /       /       /     |
  //       |        /       /       /      |
  //   |   *   |   *   |   *   |   *   |   *   |
  //   -----------------------------------------
  //   |   0   |   1   |   2   |   3   |   4   |
  //   -----------------------------------------
  //           MUTABLE TOKEN POINTER LIST
  //
  // How do we delete token `a`? The obvious (but subtly incorrect) answer is to call
  // |DropToken(a)|, but this will be buggy, as the |start()| token will now be be unrepresented in
  // the token pointer list. Instead, we first need to |UpdateTokenPointer(&start(), b)|, to ensure
  // that the pointer in the token pointer list for `b` is aimed at a (new) copy of `b` living the
  // at |start()|. The resulting state of affairs, after calling |UpdateTokenPointer(&start(), b);
  // DropToken(a);| is:
  //
  //     RAW AST NODE          SOURCE TOKEN STASH
  //   -----------------    -------------------------
  //   |start()| end() |    |   0   |   1   |   2   |
  //   -----------------    -------------------------
  //   |   b'  |   e   |    |   b   |   c   |   d   |
  //       |       |                  /       /
  //       |       |                 /       /
  //       |       |                /       /
  //       |       |               /       /
  //       |       |              /       /
  //       |       |             /       /
  //       |        -----------------------
  //        \                  /       /   |
  //          \               /       /    |
  //            \            /       /     |
  //              \         /       /      |
  //   |   X   |   *   |   *   |   *   |   *   |
  //   -----------------------------------------
  //   |   X   |   0   |   1   |   2   |   3   |
  //   -----------------------------------------
  //           MUTABLE TOKEN POINTER LIST
  //
  bool UpdateTokenPointer(Token* raw_ast_token_ptr, TokenIterator new_token_it) {
    Token* new_token_ptr = *new_token_it;

    // Find the pointer to the |raw_ast_token_ptr|'s memory in this |TokenSlice|. We want to make
    // sure that this still points to the correct |Token| (that |Token| hasn't been removed yet,
    // after all), but not into the raw AST, since that |Token| is being updated to a new value.
    std::optional<TokenIterator> maybe_found_it = SearchForward(
        [&](const Token* entry) { return entry->span() == raw_ast_token_ptr->span(); });
    if (!maybe_found_it.has_value()) {
      return false;
    }

    // Take the value of the raw AST |Token| pointed to by |raw_ast_token_ptr|, and add it to the
    // |token_stash_|.
    token_stash_.emplace_back(std::make_unique<Token>(*raw_ast_token_ptr));

    // The pointer in the |TokenSlice| that previously pointed at this raw AST node is updated to
    // point to the newly created |Token| entry. The ensures that the |Token| is still retained in
    // the correct position in the |MutableTokenPointerList|, even if it is no longer represented in
    // the raw AST itself. To keep this |Token| around, we need a new home for it where we can point
    // to, so we append it to the |token_stash_| instead.
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

  // Insert a new |Token| into the |TokenPointerList|, directly after another |Token|. Upon
  // completion of this operation, the |Token| may be dereferenced from the resulting iterator for
  // insertion into the raw AST.
  TokenIterator AddTokenAfter(TokenIterator prev_it, const std::string& text, Token::Kind kind,
                              Token::Subkind subkind) {
    return AddTokenBefore(prev_it + 1, text, kind, subkind);
  }

  // Insert a new |Token| into the |TokenPointerList|, directly before another |Token|. Upon
  // completion of this operation, the |Token| may be dereferenced from the resulting iterator for
  // insertion into the raw AST.
  TokenIterator AddTokenBefore(TokenIterator next_it, const std::string& text, Token::Kind kind,
                               Token::Subkind subkind) {
    // Make sure the token we are inserting before is after the |kStartOFile| token, but still in
    // the underlying vector. Also disallow inserting a token after the |kEndOfFile| token.
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

    // TODO(fxbug.dev/114357): As noted in the comment on |TokenIterator|, the following operation
    // has the potential to cause a re-allocation of the underlying storage, thereby invalidating
    // all |TokenIterator|s (including |prev_it|, |next_it|, and |new_token_it| here, as well as any
    // other |TokenIterator|s minted before this method was called). We currently pre-allocate a
    // large vector to try and avoid this, and use the following assert to error fast at runtime,
    // but better solutions are possible.
    auto ptr_list_location = underlying_.begin();
    underlying_.insert(next_it, token_stash_.back().get());
    ZX_ASSERT(ptr_list_location == underlying_.begin());

    // The |new_token_it| points the position of the previous |next_it|, which now has to be
    // incremented to continue pointing at the same |Token*| entry as before.
    TokenIterator new_token_it = next_it;
    next_it++;
    end_++;

    // Walk forward, incrementing the |sub_ordinal| of every entry that has the same |ordinal| as
    // the |SyntheticToken| we just created. This will keep our |MutableTokenPointerList| properly
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

 private:
  std::vector<std::unique_ptr<std::string>>& data_stash_;
  std::vector<std::unique_ptr<Token>>& token_stash_;
  MutableTokenPointerList& underlying_;
  int64_t begin_;
  int64_t end_;
};

namespace {

using MutableElementMap = std::unordered_map<raw::SourceElement::Signature, raw::SourceElement*>;

// A generator class that takes an immutable raw AST and, through some icky const_cast magic, turns
// it into a mutable one.
class MutableElementMapGenerator : public raw::DeclarationOrderTreeVisitor {
 public:
  MutableElementMapGenerator() : map_(std::make_unique<MutableElementMap>()) {}

  std::unique_ptr<MutableElementMap> Produce(const std::unique_ptr<raw::File>& raw_ast) {
    raw::DeclarationOrderTreeVisitor::OnFile(raw_ast);
    return std::move(map_);
  }

 private:
  void OnSourceElementStart(const raw::SourceElement& element) override {
    auto mutable_element = const_cast<raw::SourceElement*>(&element);
    map_->insert({mutable_element->source_signature(), mutable_element});
    raw::DeclarationOrderTreeVisitor::OnSourceElementStart(element);
  }

  std::unique_ptr<MutableElementMap> map_;
};

// This class helps do some automatic cleanup after user-supplied |When*| methods perform a
// transformation. It should be constructed prior to each |When*| call, and have its relevant |On*|
// method called on the mutated raw AST node as immediately after that call returns, like:
//
//   TokenPointerResolver resolver = GetTokenPointerResolver(&token_slice);
//   WhenFoo(mut, token_slice);
//   resolver.OnFoo(*mut);
//
// When calling an |Add*| method on a |TokenSlice|, a user creates a new |Token| stored in the
// owning |Transformer|s token stash, and an entry in the underlying |MutableTokenPointerList| that
// points to that that stashed |Token|. If they later insert the returned |Token| into the raw AST,
// the |Token| pointed to be the aforementioned entry in |MutableTokenPointerList| will NOT be the
// one in the raw AST (that's a copy), but rather the original one in the stash. This is mostly
// okay, but can cause problems if the newly injected |Token| has its ordinal changed, as the copy
// in the raw AST will not be updated.
//
// To fix this problem, the |TokenPointerResolver| walks all the |Token|s held in the raw AST proper
// (that is, represented in at least one raw AST node's |start()| or |end()| field) and makes sure
// that it's corresponding entry in the |MutableTokenPointerList| points into the raw AST instead of
// the |Transformers| token stash.
class TokenPointerResolver : public raw::DeclarationOrderTreeVisitor {
 public:
  explicit TokenPointerResolver(TokenSlice* token_slice) : token_slice_(token_slice) {}

 private:
  void OnSourceElementStart(const raw::SourceElement& element) override {
    OnToken(&element.start());
  }

  void OnSourceElementEnd(const raw::SourceElement& element) override { OnToken(&element.end()); }

  void OnToken(const Token* token) {
    // TODO(fxbug.dev/114357): Searching the entire |TokenSlice| for each |Token|, every time we
    // make a change, has N^2 complexity, where N is the number of |SourceElement|s we visit. This
    // is a good place to look for future optimizations.

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

  TokenSlice* token_slice_;
};

// Tracks the state of the transformation process for a single |SourceFile|.
class FileTransformState {
 public:
  FileTransformState(const SourceFile* source_file, std::unique_ptr<raw::File> immutable_raw_ast,
                     std::unique_ptr<raw::File> mutable_raw_ast,
                     std::unique_ptr<MutableElementMap> mutable_element_map)
      : source_file(source_file),
        immutable_raw_ast(std::move(immutable_raw_ast)),
        mutable_raw_ast(std::move(mutable_raw_ast)),
        mutable_element_map(std::move(mutable_element_map)) {
    auto token_ptr_list = raw::TokenPointerListBuilder(this->mutable_raw_ast).Build();
    mutable_token_ptr_list = std::make_unique<MutableTokenPointerList>();

    // TODO(fxbug.dev/114357): This is a hack to work around the limitations of |TokenIterator|
    // described above. We simply guess that the number of |Token|s in the list won't double due to
    // the transformations we're doing.
    mutable_token_ptr_list->reserve(std::max(token_ptr_list.size() * 2, static_cast<size_t>(100)));

    for (const Token* token_ptr : token_ptr_list) {
      mutable_token_ptr_list->push_back(const_cast<Token*>(token_ptr));
    }
  }

  // The |SourceFile| being transformed.
  const SourceFile* source_file;
  // An immutable copy of the raw AST derived from |source_file|. We'll use this to walk the tree
  // in a predictable manner, ensuring that it won't change under us as we mutate it.
  std::unique_ptr<raw::File> immutable_raw_ast;
  // The actual raw AST that we'll be transforming.
  std::unique_ptr<raw::File> mutable_raw_ast;
  // The |TokenPointerList| that points to all of the tokens held in |mutable_raw_ast|. These two
  // fields must be updated in tandem.
  std::unique_ptr<MutableTokenPointerList> mutable_token_ptr_list;
  // A map from raw AST nodes held in |immutable_raw_ast| to those in |mutable_raw_ast|. At every
  // stop along our walk of |immutable_raw_ast|, we'll use the signature of the node we're currently
  // stopped at to get the corresponding node in the |mutable_raw_ast| that we're actually
  // transforming.
  std::unique_ptr<MutableElementMap> mutable_element_map;
};

// An enumeration of steps that transforms take as they progress.
enum struct Step {
  // The starting state.
  kNew,
  // The |Prepare()| operation has started.
  kPreparing,
  // The |Prepare()| operation has finished successfully.
  kPrepared,
  // The |Transform()| operation has started.
  kTransforming,
  // The |Transform()| operation has finished successfully.
  kTransformed,
  // The |Format()| operation has started.
  kFormatting,
  // The |Format()| operation has finished successfully.
  kSuccess,
  // Any one of the steps finished with errors.
  kFailure,
};

// A transformation error.
struct Error {
  Step step;
  std::string msg;
};

// A base-class that can be derived from to build transformers that alter FIDL source code in some
// structured way. When users extend this class, they are expected to provide one or more override
// for the protected |When*| methods which modify the raw AST nodes they visit.
//
// ## Usage
//
// This class is is meant to be extended by end users (via either of its derived classes,
// |ParsedTransformer| or |CompiledTransformer|) to make a |FooTransformer|. Each protected
// |When*()| method available on this class is called for its relevant |NodeKind| in turn. The raw
// AST is traversed in declaration order, with each declaration tree being visited using post-order
// traversal (importantly, this means that children are always visited before parents). The
// |source_files| vector passed into this class always has its files processed in the order they are
// supplied.
//
// ## Best practices
//
// It is good practice to keep |Transformer|s relatively small and focused: rather than trying to
// rewrite the whole AST on one go, it's better to make a small change, print it, make the next one,
// etc. Transformations can be a bit difficult to reason about, since any non-trivial transform will
// likely necessitate touching multiple nodes in specific order ("first I modify the |Identifier|,
// then its owning |CompoundIdentifier|, then..."). There can also be subtle edge cases, especially
// when comments are involved - combining multiple transformations in a single pass makes things
// much more convoluted.
//
// A good rule of thumb is that the actual transformations should occur at the "highest" node that
// the |Token| being transformed appears in the raw AST. For example, suppose we want to add the
// `resource` modifier to all `union` declarations. A naive approach would be to use the
// |WhenUnionDeclaration()| method in our |Transformer|-inheriting class, but this will be
// erroneous. Consider the following FIDL:
//
//   type U = flexible union {};
//            ^^^^^^^^
//
// The marked `flexible` token is actually the |start()|ing |Token| of *4* raw AST nodes: a
// |raw::TypeConstructor|, which contains a |raw::InlineLayoutReference|, which contains a
// |raw::Layout|, which contains a |raw::Modifiers|. If we want to prepend a `resource` token to our
// token list, we will need to make sure we update all of these |start()| values. Because AST nodes
// are traversed using post-order traversal, we know we will see nodes higher up the tree last,
// making them the best place to apply the actual transformation. In this instance, we should handle
// the actual transform in |WhenTypeConstructor()| instead, since this node will have a view into
// all of its children, allowing all necessary |Token| updates to be made at once.
//
// For an example of this specific case in action, check out the |AddModifierToTypeDeclTransformer|
// in |parsed_transformer_tests.cc|.
class Transformer : public raw::DeclarationOrderTreeVisitor {
 public:
  Transformer(const std::vector<const SourceFile*>& source_files,
              const fidl::ExperimentalFlags& experimental_flags, Reporter* reporter)
      : reporter_(reporter),
        source_files_(source_files),
        experimental_flags_(experimental_flags),
        reporter_was_already_silenced_(reporter->silence_fixables()) {
    // No reporting of other fixable errors - we don't want one fixable error to derail to fixing of
    // another one.
    reporter_->set_silence_fixables(true);
  }
  ~Transformer() override {
    if (!reporter_was_already_silenced_) {
      reporter_->set_silence_fixables(false);
    }
  }

  // Handles preparation (input parsing, map building, etc) for the specified transformer
  // derivation. If any of these steps fail, it means that the inputs were incorrect (missing files,
  // parse errors, etc).
  virtual bool Prepare() = 0;

  // Performs the actual transformations, modifying the each raw AST + token list pair in sync to
  // properly reflect the new, post-transformation state. Must be followed by a call to |Format| to
  // take the now transformed data and re-print it back into source strings.
  bool Transform() {
    ZX_ASSERT(step_ == Step::kPrepared);
    NextStep();

    for (auto& transforming : transform_states_) {
      auto ptr_list_location = CurrentlyTransforming().mutable_token_ptr_list->begin();
      OnFile(transforming.immutable_raw_ast);
      if (HasErrors()) {
        AddError("Error while transforming " + std::string(transforming.source_file->filename()) +
                 "\n");
      }

      // TODO(fxbug.dev/114357): as noted in the comment on |TokenIterator|, we currently
      // pre-allocate a large vector to try and avoid this, and use the following assert to error
      // fast at runtime, but better solutions are possible.
      ZX_ASSERT(ptr_list_location == CurrentlyTransforming().mutable_token_ptr_list->begin());

      index_++;
    }

    if (HasErrors()) {
      return false;
    }
    NextStep();
    return true;
  }

  // After transforming successfully, we want to loop back through and format each transformed file.
  // If there is an error here (ie, at least one of the files fails to format), we know that there
  // was very likely some error in the transformation logic (or, less likely, the formatter itself
  // is broken).
  //
  // The formatted file strings are returned in the same order that the original |SourceFiles| were
  // supplied to this class.
  std::optional<std::vector<std::string>> Format() {
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

      // TODO(fxbug.dev/114357): Currently, breakages here will be very opaque. We should improve
      // this to report more useful errors when formatting fails.
      std::optional<std::string> pretty_printed =
          formatter.Format(std::move(transformed.mutable_raw_ast), token_ptr_list,
                           original_source_file->data().size());
      if (!pretty_printed.has_value()) {
        AddError("Failed to pretty print " + (std::string(original_source_file->filename())) +
                 "\n");
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

  bool HasErrors() { return !errors_.empty(); }

  const std::vector<Error>& GetErrors() { return errors_; }

  // The overrides specified here are common to both the derived wrapper classes,
  // |ParsedTransformer| and |CompiledTransformer|. In other words, none of these raw AST nodes map
  // cleanly to flat AST nodes, so the methods would be identical on both the parsed and compiled
  // transformer, and are a thus a better fit here to avoid repetition.
 protected:
  virtual void WhenAttributeList(raw::AttributeList* element, TokenSlice&) {}
  virtual void WhenCompoundIdentifier(raw::CompoundIdentifier* element, TokenSlice&) {}
  virtual void WhenFile(raw::File* element, TokenSlice&) {}
  virtual void WhenIdentifier(raw::Identifier* element, TokenSlice&) {}
  virtual void WhenInlineLayoutReference(raw::InlineLayoutReference* element, TokenSlice&) {}
  virtual void WhenLibraryDeclaration(raw::LibraryDeclaration* element, TokenSlice&) {}
  virtual void WhenNamedLayoutReference(raw::NamedLayoutReference* element, TokenSlice&) {}
  virtual void WhenModifiers(raw::Modifiers* element, TokenSlice&) {}
  virtual void WhenOrdinal64(raw::Ordinal64* element, TokenSlice&) {}
  virtual void WhenParameterList(raw::ParameterList* element, TokenSlice&) {}
  virtual void WhenTypeDeclaration(raw::TypeDeclaration* element, TokenSlice&) {}
  virtual void WhenUsing(raw::Using* element, TokenSlice&) {}

 private:
  // Mutable internal state tracking the transformation as it progresses, made visible to
  // intermediate derivations like |ParsedTransformer| and |CompiledTransformer| via "protected"
  // accessors.
  Reporter* reporter_;
  Step step_ = Step::kNew;

  // All of the methods in this "protected" block should be privatized in |ParsedTransformer| and
  // |CompiledTransformer|, so that derivations of those classes no longer have access to them.
 protected:
  Reporter* reporter() { return reporter_; }
  Step step() { return step_; }

  // User-visible function for reporting an error state during transformation. The transformer will
  // proceed as before, though no printing will occur.
  void AddError(const std::string& msg) {
    errors_.push_back(Error{
        .step = step_,
        .msg = msg,
    });
  }

  // This operation readies the passed in source files for transformation. Specifically, it parses
  // each of them, and returns false if any (non-fixable) errors were reported during that process,
  // indicating that the files have problems that need to be fixed before they can be run through
  // a transformer.
  //
  // For each entry in the given |source_files| vector, create a corresponding |FileTransformState|
  // to track transformation progress as we iterate over files. This will require parsing the source
  // file twice: once to create an immutable raw AST to walk over, and a second time to make a
  // mutable copy to transform.
  bool BuildTransformStates() {
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

      // We have two identical raw ASTs, derived from the same |SourceFile| in memory. We'll now
      // walk the second one, turning it into a map that we may access from the first as we visit
      // each of its nodes.
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

  FileTransformState& CurrentlyTransforming() { return transform_states_[index_]; }

  // Get the mutable copy of the raw AST node currently being visited. The supplied |el| argument
  // must be a pointer into the existing, immutable raw AST, not the mutable one.
  raw::SourceElement* GetAsMutable(const raw::SourceElement* el) {
    // Get the correct mutable mirror element.
    auto mut = CurrentlyTransforming().mutable_element_map->find(el->source_signature());
    ZX_ASSERT_MSG(mut != CurrentlyTransforming().mutable_element_map->end(),
                  "mutable copy of raw AST element not found");
    return mut->second;
  }

  // Create a |TokenSlice|. Note that the start and end |Token| passed to this function are
  // inclusive.
  //
  // TODO(fxbug.dev/114357): There is certainly a way to make this faster by not searching the
  // entire |MutableTokenPointerList| every time we call this, and instead keeping some state
  // tracking what range in the list we are looking at for each |SourceElement|, but this is an
  // optimization for later.
  TokenSlice GetTokenSlice(Token from, Token until) {
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

  // Must be called before every |When*| method call, so that any user modifications to the raw AST
  // are "resolved" properly (see the comment on |TokenPointerResolver| for more information).
  static TokenPointerResolver GetTokenPointerResolver(TokenSlice* token_slice) {
    return TokenPointerResolver(token_slice);
  }

  void NextStep() {
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

  // Parse a source file. A return value of |nullptr| means that the parse failed, and that all the
  // interesting details will be found in the |reporter|.
  static std::unique_ptr<raw::File> ParseSource(const SourceFile* source_file,
                                                const fidl::ExperimentalFlags& experimental_flags,
                                                Reporter* reporter) {
    fidl::Lexer lexer(*source_file, reporter);
    Parser parser(&lexer, reporter, experimental_flags);
    std::unique_ptr<raw::File> ast = parser.Parse();
    if (!parser.Success()) {
      return nullptr;
    }

    return ast;
  }

  // Const parameters ingested at construction time.
  const std::vector<const SourceFile*> source_files_;
  const fidl::ExperimentalFlags& experimental_flags_;
  const bool reporter_was_already_silenced_;

  // The visitation methods inherited from |DeclarationOrderTreeVisitor| are intentionally hidden,
  // so that users extending this class cannot mess with the transformation state these methods are
  // entrusted to manage. User specified transforms should be done via the corresponding |When*|
  // methods instead.
  //
  // The overrides specified here are common to both the derived wrapper classes,
  // |ParsedTransformer| and |CompiledTransformer|. In other words, none of these raw AST nodes map
  // cleanly to flat AST nodes, so the methods would be identical on both the parsed and compiled
  // transformer, and are a thus a better fit here to avoid repetition.
 private:
  void OnAttributeList(const std::unique_ptr<raw::AttributeList>& el) final {
    VISIT_THEN_TRANSFORM_UNIQUE_PTR(AttributeList, AttributeList);
  }
  void OnCompoundIdentifier(const std::unique_ptr<raw::CompoundIdentifier>& el) final {
    VISIT_THEN_TRANSFORM_UNIQUE_PTR(CompoundIdentifier, CompoundIdentifier);
  }
  void OnFile(const std::unique_ptr<raw::File>& el) final {
    VISIT_THEN_TRANSFORM_UNIQUE_PTR(File, File);
  }
  void OnIdentifier(const std::unique_ptr<raw::Identifier>& el) final {
    VISIT_THEN_TRANSFORM_UNIQUE_PTR(Identifier, Identifier);
  }
  void OnInlineLayoutReference(const std::unique_ptr<raw::InlineLayoutReference>& el) final {
    VISIT_THEN_TRANSFORM_UNIQUE_PTR(InlineLayoutReference, InlineLayoutReference);
  }
  void OnLibraryDeclaration(const std::unique_ptr<raw::LibraryDeclaration>& el) final {
    VISIT_THEN_TRANSFORM_UNIQUE_PTR(LibraryDeclaration, LibraryDeclaration);
  }
  void OnNamedLayoutReference(const std::unique_ptr<raw::NamedLayoutReference>& el) final {
    VISIT_THEN_TRANSFORM_UNIQUE_PTR(NamedLayoutReference, NamedLayoutReference);
  }
  void OnOrdinal64(raw::Ordinal64& el) final { VISIT_THEN_TRANSFORM_REFERENCE(Ordinal64); }
  void OnModifiers(const std::unique_ptr<raw::Modifiers>& el) final {
    VISIT_THEN_TRANSFORM_UNIQUE_PTR(Modifiers, Modifiers);
  }
  void OnParameterList(const std::unique_ptr<raw::ParameterList>& el) final {
    VISIT_THEN_TRANSFORM_UNIQUE_PTR(ParameterList, ParameterList);
  }
  void OnTypeDeclaration(const std::unique_ptr<raw::TypeDeclaration>& el) final {
    VISIT_THEN_TRANSFORM_UNIQUE_PTR(TypeDeclaration, TypeDeclaration);
  }
  void OnUsing(const std::unique_ptr<raw::Using>& el) final {
    VISIT_THEN_TRANSFORM_UNIQUE_PTR(Using, Using);
  }

  // The following methods are all finalized to prevent derived classes from overriding them.
  void OnConstant(const std::unique_ptr<raw::Constant>& el) final {
    raw::DeclarationOrderTreeVisitor::OnConstant(el);
  }
  void OnLayoutMember(const std::unique_ptr<raw::LayoutMember>& el) final {
    raw::DeclarationOrderTreeVisitor::OnLayoutMember(el);
  }
  void OnLayoutParameter(const std::unique_ptr<raw::LayoutParameter>& el) final {
    raw::DeclarationOrderTreeVisitor::OnLayoutParameter(el);
  }
  void OnLayoutReference(const std::unique_ptr<raw::LayoutReference>& el) final {
    raw::DeclarationOrderTreeVisitor::OnLayoutReference(el);
  }
  void OnLiteral(const std::unique_ptr<raw::Literal>& el) final {
    raw::DeclarationOrderTreeVisitor::OnLiteral(el);
  }
  void OnSourceElementEnd(const raw::SourceElement& el) final {
    raw::DeclarationOrderTreeVisitor::OnSourceElementEnd(el);
  }
  void OnSourceElementStart(const raw::SourceElement& el) final {
    raw::DeclarationOrderTreeVisitor::OnSourceElementStart(el);
  }

  // Mutable internal state tracking the transformation as it progresses, not visible to
  // intermediate derivations.
  std::vector<FileTransformState> transform_states_;
  size_t index_ = 0;
  std::optional<TokenIterator> current_slice_ = std::nullopt;
  std::vector<std::unique_ptr<std::string>> data_stash_;
  std::vector<std::unique_ptr<Token>> token_stash_;
  std::vector<Error> errors_;
};

}  // namespace

// A |Transformer| that transforms a parsed raw AST, with no compilation (aka flat AST) information
// available. Files passed to this class do not necessarily need to compile - they just need to
// parse successfully. See the description on |Transformer| for more information.
//
// Implementors are expected to use this class by deriving it to make their own |*Transformer|,
// with the appropriate |When*| methods overwritten.
class ParsedTransformer : public Transformer {
 public:
  ParsedTransformer(const std::vector<const SourceFile*>& source_files,
                    const fidl::ExperimentalFlags& experimental_flags, Reporter* reporter)
      : Transformer(source_files, experimental_flags, reporter) {}

  // This operation readies the passed in source files for transformation. Specifically, it parses
  // each of them, and returns false if any (non-fixable) errors were reported during that process,
  // indicating that the files have problems that need to be fixed before they can be run through
  // a transformer.
  //
  // For each entry in the given |source_files| vector, create a corresponding |FileTransformState|
  // to track transformation progress as we iterate over files. This will require parsing the source
  // file twice: once to create an immutable raw AST to walk over, and a second time to make a
  // mutable copy to transform.
  bool Prepare() final;

 protected:
  // Derivations of this class should override at least one of these noop methods to perform the
  // actual transforms in question. Each such method receives two arguments: a mutable copy of the
  // raw AST node to be transformed, and a slice of |Token| pointers that occur between the
  // |start()| and |end()| token of that raw AST node, inclusive. Transforms are implemented by
  // adjusting these two data structures in tandem, taking care to ensure that the changes applied
  // are reflected in both.
  virtual void WhenAliasDeclaration(raw::AliasDeclaration*, TokenSlice&) {}
  virtual void WhenAttributeArg(raw::AttributeArg*, TokenSlice&) {}
  virtual void WhenAttribute(raw::Attribute*, TokenSlice&) {}
  virtual void WhenBinaryOperatorConstant(raw::BinaryOperatorConstant*, TokenSlice&) {}
  virtual void WhenBitsDeclaration(raw::Layout*, TokenSlice&) {}
  virtual void WhenBitsMember(raw::ValueLayoutMember*, TokenSlice&) {}
  virtual void WhenBoolLiteral(raw::BoolLiteral*, TokenSlice&) {}
  virtual void WhenConstDeclaration(raw::ConstDeclaration*, TokenSlice&) {}
  virtual void WhenDocCommentLiteral(raw::DocCommentLiteral*, TokenSlice&) {}
  virtual void WhenEnumDeclaration(raw::Layout*, TokenSlice&) {}
  virtual void WhenEnumMember(raw::ValueLayoutMember*, TokenSlice&) {}
  virtual void WhenIdentifierConstant(raw::IdentifierConstant*, TokenSlice&) {}
  virtual void WhenIdentifierLayoutParameter(raw::IdentifierLayoutParameter*, TokenSlice&) {}
  virtual void WhenLayoutParameterList(raw::LayoutParameterList*, TokenSlice&) {}
  virtual void WhenLiteralConstant(raw::LiteralConstant*, TokenSlice&) {}
  virtual void WhenLiteralLayoutParameter(raw::LiteralLayoutParameter*, TokenSlice&) {}
  virtual void WhenNumericLiteral(raw::NumericLiteral*, TokenSlice&) {}
  virtual void WhenProtocolCompose(raw::ProtocolCompose*, TokenSlice&) {}
  virtual void WhenProtocolDeclaration(raw::ProtocolDeclaration*, TokenSlice&) {}
  virtual void WhenProtocolMethod(raw::ProtocolMethod*, TokenSlice&) {}
  virtual void WhenResourceDeclaration(raw::ResourceDeclaration*, TokenSlice&) {}
  virtual void WhenResourceProperty(raw::ResourceProperty*, TokenSlice&) {}
  virtual void WhenServiceDeclaration(raw::ServiceDeclaration*, TokenSlice&) {}
  virtual void WhenServiceMember(raw::ServiceMember*, TokenSlice&) {}
  virtual void WhenStringLiteral(raw::StringLiteral*, TokenSlice&) {}
  virtual void WhenStructDeclaration(raw::Layout*, TokenSlice&) {}
  virtual void WhenStructMember(raw::StructLayoutMember*, TokenSlice&) {}
  virtual void WhenTableDeclaration(raw::Layout*, TokenSlice&) {}
  virtual void WhenTableMember(raw::OrdinaledLayoutMember*, TokenSlice&) {}
  virtual void WhenTypeConstraints(raw::TypeConstraints*, TokenSlice&) {}
  virtual void WhenTypeConstructor(raw::TypeConstructor*, TokenSlice&) {}
  virtual void WhenTypeLayoutParameter(raw::TypeLayoutParameter*, TokenSlice&) {}
  virtual void WhenUnionDeclaration(raw::Layout*, TokenSlice&) {}
  virtual void WhenUnionMember(raw::OrdinaledLayoutMember*, TokenSlice&) {}

 private:
  // The visitation methods inherited from |DeclarationOrderTreeVisitor| are intentionally hidden,
  // so that users extending this class cannot mess with the transformation state they are entrusted
  // to manage. User specified transforms should be done via the corresponding |When*| methods
  // instead.
  void OnAliasDeclaration(const std::unique_ptr<raw::AliasDeclaration>&) final;
  void OnAttributeArg(const std::unique_ptr<raw::AttributeArg>&) final;
  void OnAttribute(const std::unique_ptr<raw::Attribute>&) final;
  void OnBinaryOperatorConstant(const std::unique_ptr<raw::BinaryOperatorConstant>&) final;
  void OnBoolLiteral(raw::BoolLiteral&) final;
  void OnConstDeclaration(const std::unique_ptr<raw::ConstDeclaration>&) final;
  void OnDocCommentLiteral(raw::DocCommentLiteral&) final;
  void OnIdentifierConstant(const std::unique_ptr<raw::IdentifierConstant>&) final;
  void OnIdentifierLayoutParameter(const std::unique_ptr<raw::IdentifierLayoutParameter>&) final;
  void OnLayout(const std::unique_ptr<raw::Layout>&) final;
  void OnLayoutParameterList(const std::unique_ptr<raw::LayoutParameterList>&) final;
  void OnLiteralConstant(const std::unique_ptr<raw::LiteralConstant>&) final;
  void OnLiteralLayoutParameter(const std::unique_ptr<raw::LiteralLayoutParameter>&) final;
  void OnNumericLiteral(raw::NumericLiteral&) final;
  void OnOrdinaledLayoutMember(const std::unique_ptr<raw::OrdinaledLayoutMember>&) final;
  void OnProtocolCompose(const std::unique_ptr<raw::ProtocolCompose>&) final;
  void OnProtocolDeclaration(const std::unique_ptr<raw::ProtocolDeclaration>&) final;
  void OnProtocolMethod(const std::unique_ptr<raw::ProtocolMethod>&) final;
  void OnResourceDeclaration(const std::unique_ptr<raw::ResourceDeclaration>&) final;
  void OnResourceProperty(const std::unique_ptr<raw::ResourceProperty>&) final;
  void OnServiceDeclaration(const std::unique_ptr<raw::ServiceDeclaration>&) final;
  void OnServiceMember(const std::unique_ptr<raw::ServiceMember>&) final;
  void OnStringLiteral(raw::StringLiteral&) final;
  void OnStructLayoutMember(const std::unique_ptr<raw::StructLayoutMember>&) final;
  void OnTypeConstraints(const std::unique_ptr<raw::TypeConstraints>&) final;
  void OnTypeConstructor(const std::unique_ptr<raw::TypeConstructor>&) final;
  void OnTypeLayoutParameter(const std::unique_ptr<raw::TypeLayoutParameter>&) final;
  void OnValueLayoutMember(const std::unique_ptr<raw::ValueLayoutMember>&) final;

  // Privatize some previously "protected" methods from the base class, so that derived classes
  // cannot access them.
  using Transformer::BuildTransformStates;
  using Transformer::CurrentlyTransforming;
  using Transformer::GetAsMutable;
  using Transformer::GetTokenPointerResolver;
  using Transformer::GetTokenSlice;
  using Transformer::NextStep;
  using Transformer::ParseSource;
  using Transformer::reporter;
  using Transformer::step;
};

// A |Transformer| that transforms an also-compiled raw AST. Implementors are expected to use this
// class by deriving it to make their own |*Transformer|, with the appropriate |When*| methods
// overwritten. Each |When*| method received mutable access to the underlying |SourceElement| it is
// visiting, as well as a mutable |TokenSlice| representing the |Token|s contained therein.
// Additionally, it receives an immutable |SourceMapEntry| containing all known flat AST nodes that
// were "sourced" from the |SourceElement| being visited.
//
// Users are expected to pass in a list of |SourceFile|s for the library being converted, plus an
// optional list of (topologically sorted) dependency |SourceFile| lists for libraries that require
// them, at construction time.
class CompiledTransformer : public Transformer {
 public:
  CompiledTransformer(const std::vector<const SourceFile*>& library_source_files,
                      const std::vector<std::vector<const SourceFile*>>& dependencies_source_files,
                      const fidl::ExperimentalFlags& experimental_flags, Reporter* reporter)
      : Transformer(library_source_files, experimental_flags, reporter),
        virtual_file_("generated"),
        all_libraries_(reporter, &virtual_file_),
        dependencies_source_files_(dependencies_source_files) {}
  CompiledTransformer(const std::vector<const SourceFile*>& library_source_files,
                      const fidl::ExperimentalFlags& experimental_flags, Reporter* reporter)
      : CompiledTransformer(library_source_files, {}, experimental_flags, reporter) {}

  // This operation readies the passed in source files for transformation. It performs all of the
  // same operations as |ParsedTransformer::Prepare()|, but then it also compiles the files and
  // produces a |SourceMap| as well.
  bool Prepare() final;

 protected:
  // Derivations of this class should override at least one of these noop methods to perform the
  // actual transforms in question. Each such method receives three arguments: a mutable copy of the
  // raw AST node to be transformed, a slice of |Token| pointers that occur between the |start()|
  // and |end()| token of that raw AST node, inclusive, and finally an immutable |SourceMapEntry|
  // containing all flat AST nodes derived from the raw AST node being visited. Transforms are
  // implemented by adjusting the first two data structures in tandem, taking care to ensure that
  // the changes applied are reflected in both.
  virtual void WhenAliasDeclaration(raw::AliasDeclaration*, TokenSlice&,
                                    const VersionedEntry<flat::Alias>*) {}
  virtual void WhenAttributeArg(raw::AttributeArg*, TokenSlice&,
                                const UniqueEntry<flat::AttributeArg>*) {}
  virtual void WhenAttribute(raw::Attribute*, TokenSlice&, const UniqueEntry<flat::Attribute>*) {}
  virtual void WhenBinaryOperatorConstant(raw::BinaryOperatorConstant*, TokenSlice&,
                                          const UniqueEntry<flat::BinaryOperatorConstant>*) {}
  virtual void WhenBitsDeclaration(raw::Layout*, TokenSlice&, const VersionedEntry<flat::Bits>*) {}
  virtual void WhenBitsMember(raw::ValueLayoutMember*, TokenSlice&,
                              const VersionedEntry<flat::Bits::Member>*) {}
  virtual void WhenBoolLiteral(raw::BoolLiteral*, TokenSlice&,
                               const UniqueEntry<flat::LiteralConstant>*) {}
  virtual void WhenConstDeclaration(raw::ConstDeclaration*, TokenSlice&,
                                    const VersionedEntry<flat::Const>*) {}
  virtual void WhenDocCommentLiteral(raw::DocCommentLiteral*, TokenSlice&,
                                     const UniqueEntry<flat::LiteralConstant>*) {}
  virtual void WhenEnumDeclaration(raw::Layout*, TokenSlice&, const VersionedEntry<flat::Enum>*) {}
  virtual void WhenEnumMember(raw::ValueLayoutMember*, TokenSlice&,
                              const VersionedEntry<flat::Enum::Member>*) {}
  virtual void WhenIdentifierConstant(raw::IdentifierConstant*, TokenSlice&,
                                      const UniqueEntry<flat::IdentifierConstant>*) {}
  virtual void WhenIdentifierLayoutParameter(raw::IdentifierLayoutParameter*, TokenSlice&,
                                             const UniqueEntry<flat::IdentifierLayoutParameter>*) {}
  virtual void WhenLayoutParameterList(raw::LayoutParameterList*, TokenSlice&,
                                       const UniqueEntry<flat::LayoutParameterList>*) {}
  virtual void WhenLiteralLayoutParameter(raw::LiteralLayoutParameter*, TokenSlice&,
                                          const UniqueEntry<flat::LiteralLayoutParameter>*) {}
  virtual void WhenNumericLiteral(raw::NumericLiteral*, TokenSlice&,
                                  const UniqueEntry<flat::LiteralConstant>*) {}
  virtual void WhenProtocolCompose(raw::ProtocolCompose*, TokenSlice&,
                                   const VersionedEntry<flat::Protocol::ComposedProtocol>*) {}
  virtual void WhenProtocolDeclaration(raw::ProtocolDeclaration*, TokenSlice&,
                                       const VersionedEntry<flat::Protocol>*) {}
  virtual void WhenProtocolMethod(raw::ProtocolMethod*, TokenSlice&,
                                  const VersionedEntry<flat::Protocol::Method>*) {}
  virtual void WhenResourceDeclaration(raw::ResourceDeclaration*, TokenSlice&,
                                       const VersionedEntry<flat::Resource>*) {}
  virtual void WhenResourceProperty(raw::ResourceProperty*, TokenSlice&,
                                    const VersionedEntry<flat::Resource::Property>*) {}
  virtual void WhenServiceDeclaration(raw::ServiceDeclaration*, TokenSlice&,
                                      const VersionedEntry<flat::Service>*) {}
  virtual void WhenServiceMember(raw::ServiceMember*, TokenSlice&,
                                 const VersionedEntry<flat::Service::Member>*) {}
  virtual void WhenStringLiteral(raw::StringLiteral*, TokenSlice&,
                                 const UniqueEntry<flat::LiteralConstant>*) {}
  virtual void WhenStructDeclaration(raw::Layout*, TokenSlice&,
                                     const VersionedEntry<flat::Struct>*) {}
  virtual void WhenStructMember(raw::StructLayoutMember*, TokenSlice&,
                                const VersionedEntry<flat::Struct::Member>*) {}
  virtual void WhenTableDeclaration(raw::Layout*, TokenSlice&, const VersionedEntry<flat::Table>*) {
  }
  virtual void WhenTableMember(raw::OrdinaledLayoutMember*, TokenSlice&,
                               const VersionedEntry<flat::Table::Member>*) {}
  virtual void WhenTypeConstraints(raw::TypeConstraints*, TokenSlice&,
                                   const UniqueEntry<flat::TypeConstraints>*) {}
  virtual void WhenTypeConstructor(raw::TypeConstructor*, TokenSlice&,
                                   const UniqueEntry<flat::TypeConstructor>*) {}
  virtual void WhenTypeLayoutParameter(raw::TypeLayoutParameter*, TokenSlice&,
                                       const UniqueEntry<flat::TypeLayoutParameter>*) {}
  virtual void WhenUnionDeclaration(raw::Layout*, TokenSlice&, const VersionedEntry<flat::Union>*) {
  }
  virtual void WhenUnionMember(raw::OrdinaledLayoutMember*, TokenSlice&,
                               const VersionedEntry<flat::Union::Member>*) {}

 private:
  // A helper function that takes an unordered list of |SourceFile|s and compiles a |flat::Library|
  // out of them.
  bool CompileLibrary(const std::vector<const SourceFile*>&, const fidl::ExperimentalFlags&,
                      fidl::flat::Compiler*, Reporter*);

  // The visitation methods inherited from |DeclarationOrderTreeVisitor| are intentionally hidden,
  // so that users extending this class cannot mess with the transformation state they are entrusted
  // to manage. User specified transforms should be done via the corresponding |When*| methods
  // instead.
  void OnAliasDeclaration(const std::unique_ptr<raw::AliasDeclaration>&) override final;
  void OnAttributeArg(const std::unique_ptr<raw::AttributeArg>&) override final;
  void OnAttribute(const std::unique_ptr<raw::Attribute>&) override final;
  void OnBinaryOperatorConstant(const std::unique_ptr<raw::BinaryOperatorConstant>&) override final;
  void OnBoolLiteral(raw::BoolLiteral&) override final;
  void OnConstDeclaration(const std::unique_ptr<raw::ConstDeclaration>&) override final;
  void OnDocCommentLiteral(raw::DocCommentLiteral&) override final;
  void OnIdentifierConstant(const std::unique_ptr<raw::IdentifierConstant>&) override final;
  void OnIdentifierLayoutParameter(
      const std::unique_ptr<raw::IdentifierLayoutParameter>&) override final;
  void OnLayout(const std::unique_ptr<raw::Layout>&) override final;
  void OnLayoutParameterList(const std::unique_ptr<raw::LayoutParameterList>&) override final;
  void OnLiteralLayoutParameter(const std::unique_ptr<raw::LiteralLayoutParameter>&) override final;
  void OnNumericLiteral(raw::NumericLiteral&) override final;
  void OnOrdinaledLayoutMember(const std::unique_ptr<raw::OrdinaledLayoutMember>&) override final;
  void OnProtocolCompose(const std::unique_ptr<raw::ProtocolCompose>&) override final;
  void OnProtocolDeclaration(const std::unique_ptr<raw::ProtocolDeclaration>&) override final;
  void OnProtocolMethod(const std::unique_ptr<raw::ProtocolMethod>&) override final;
  void OnResourceDeclaration(const std::unique_ptr<raw::ResourceDeclaration>&) override final;
  void OnResourceProperty(const std::unique_ptr<raw::ResourceProperty>&) override final;
  void OnServiceDeclaration(const std::unique_ptr<raw::ServiceDeclaration>&) override final;
  void OnServiceMember(const std::unique_ptr<raw::ServiceMember>&) override final;
  void OnStringLiteral(raw::StringLiteral&) override final;
  void OnStructLayoutMember(const std::unique_ptr<raw::StructLayoutMember>&) override final;
  void OnTypeConstraints(const std::unique_ptr<raw::TypeConstraints>&) override final;
  void OnTypeConstructor(const std::unique_ptr<raw::TypeConstructor>&) override final;
  void OnTypeLayoutParameter(const std::unique_ptr<raw::TypeLayoutParameter>&) override final;
  void OnValueLayoutMember(const std::unique_ptr<raw::ValueLayoutMember>&) override final;

  // Mutable internal state, meant to be populated by the |Prepare()| step.
  fidl::VersionSelection version_selection_;
  fidl::VirtualSourceFile virtual_file_;
  fidl::flat::Libraries all_libraries_;
  std::unique_ptr<SourceMap> source_map_ = nullptr;

  // Const parameters ingested at construction time.
  const std::vector<std::vector<const SourceFile*>> dependencies_source_files_;

  // Privatize some previously "protected" methods from the base class, so that derived classes
  // cannot access them.
  using Transformer::BuildTransformStates;
  using Transformer::CurrentlyTransforming;
  using Transformer::GetAsMutable;
  using Transformer::GetTokenPointerResolver;
  using Transformer::GetTokenSlice;
  using Transformer::NextStep;
  using Transformer::ParseSource;
  using Transformer::reporter;
  using Transformer::step;
};

}  // namespace fidl::fix

#endif  // TOOLS_FIDL_FIDLC_INCLUDE_FIDL_TRANSFORMER_H_
