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
#define VISIT_THEN_TRANSFORM_UNIQUE_PTR(RAW_AST_NODE, WHEN)                        \
  DeclarationOrderTreeVisitor::On##RAW_AST_NODE(el);                               \
  auto mut = static_cast<raw::RAW_AST_NODE*>(GetAsMutable(el.get()));              \
  TokenSlice token_slice = GetTokenSlice(el->start(), el->end());                  \
  internal::TokenPointerResolver resolver = GetTokenPointerResolver(&token_slice); \
  When##WHEN(mut, token_slice);                                                    \
  std::unique_ptr<raw::RAW_AST_NODE> uptr(mut);                                    \
  resolver.On##RAW_AST_NODE(uptr);                                                 \
  static_cast<void>(uptr.release());

// A few visitor methods defined by |TreeVisitor| supply const references, rather than const
// |unique_ptr<...>| references, as their sole parameter. This macro does the same thing as
// |VISIT_THEN_TRANSFORM_UNIQUE_PTR| for such cases.
#define VISIT_THEN_TRANSFORM_REFERENCE(RAW_AST_NODE)                               \
  DeclarationOrderTreeVisitor::On##RAW_AST_NODE(el);                               \
  auto mut = static_cast<raw::RAW_AST_NODE*>(GetAsMutable(&el));                   \
  TokenSlice token_slice = GetTokenSlice(el.start(), el.end());                    \
  internal::TokenPointerResolver resolver = GetTokenPointerResolver(&token_slice); \
  When##RAW_AST_NODE(mut, token_slice);                                            \
  resolver.On##RAW_AST_NODE(*mut);

namespace fidl::fix {

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
// TODO(fxbug.dev/118371): This iterator currently does not survive resizing. This means that any
// |MutableTokenPointerList| expanding operation has the potential to invalidate all previously
// minted |TokenIterator|s, since the underlying memory they point to has been re-allocated. Our
// current "solution" to this problem is to simply pre-allocate a very large underlying token list
// (see the |Transformer| constructor - currently 2x the size of the original file) and asserting
// that the vector never grows beyond this size, but this is unideal. A better future solution would
// be to make |TokenIterator| a wrapper class that only tracks the offset into the underlying vector
// and has various overloads to "hide" this fact from users, thereby allowing any given instance to
// survive a reallocation of the underlying storage.
using TokenIterator = MutableTokenPointerList::iterator;

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

  // Insert a new |Token| into the |TokenPointerList|, directly after another |Token|. Upon
  // completion of this operation, the |Token| may be dereferenced from the resulting iterator for
  // insertion into the raw AST.
  TokenIterator AddTokenAfter(TokenIterator prev_it, const std::string& text, Token::Kind kind,
                              Token::Subkind subkind);

  // Insert a new |Token| into the |TokenPointerList|, directly before another |Token|. Upon
  // completion of this operation, the |Token| may be dereferenced from the resulting iterator for
  // insertion into the raw AST.
  TokenIterator AddTokenBefore(TokenIterator next_it, const std::string& text, Token::Kind kind,
                               Token::Subkind subkind);

  // Remove a |TokenIterator| from the |TokenSlice|. If the token is not represented at all in the
  // raw AST, this operation alone is sufficient. However, if the |Token| is used as a |start()| or
  // |end()| node anywhere in the raw AST, those nodes must first have |UpdateTokenPointer| called
  // on them to give them new values before this |Token| can be dropped.
  //
  // It is important to note that a single |TokenIterator| may be represented by multiple
  // |start()|/|end()| tokens in the raw AST, and all of those need to be updated before this method
  // can be safely called!
  void DropToken(TokenIterator pos);

  // A method that proves sugar for calling |DropToken| sequentially on every |Token| contained in
  // some |raw::SourceElement|. All of the rules from that method still apply to the |start()| and
  // |end()| tokens of the |raw::SourceElement| being dropped: if they are shared with any other
  // elements in the raw AST, those elements must be updated to use different |Token|s before this
  // drop occurs.
  bool DropSourceElement(raw::SourceElement* element);

  // Walk the |TokenSlice| from beginning to end, looking for the first |Token| that matches on all
  // of the supplied search parameters. Its main benefit is that it absolves the user from keeping
  // tracking of the |end()| themselves, as it will change with each mutation.
  std::optional<TokenIterator> SearchForward(TokenIterator from, TokenMatcher matcher);
  std::optional<TokenIterator> SearchForward(TokenMatcher matcher);

  // Walk the |TokenSlice| from end to beginning, looking for the first |Token| that matches on all
  // of the supplied search parameters. Its main benefit is that it absolves the user from keeping
  // tracking of the |end()| themselves, as it will change with each mutation.
  std::optional<TokenIterator> SearchBackward(TokenIterator from, TokenMatcher matcher);
  std::optional<TokenIterator> SearchBackward(TokenMatcher matcher);

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
  bool UpdateTokenPointer(Token* raw_ast_token_ptr, TokenIterator new_token_it);

 private:
  std::vector<std::unique_ptr<std::string>>& data_stash_;
  std::vector<std::unique_ptr<Token>>& token_stash_;
  MutableTokenPointerList& underlying_;
  int64_t begin_;
  int64_t end_;
};

namespace internal {

using MutableElementMap = std::unordered_map<raw::SourceElement::Signature, raw::SourceElement*>;

// A generator class that takes an immutable raw AST and, through some icky const_cast magic, turns
// it into a mutable one.
class MutableElementMapGenerator final : public raw::DeclarationOrderTreeVisitor {
 public:
  MutableElementMapGenerator() : map_(std::make_unique<MutableElementMap>()) {}

  std::unique_ptr<MutableElementMap> Produce(const std::unique_ptr<raw::File>& raw_ast);

 private:
  void OnSourceElementStart(const raw::SourceElement& element) final;

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
class TokenPointerResolver final : public raw::DeclarationOrderTreeVisitor {
 public:
  explicit TokenPointerResolver(TokenSlice* token_slice) : token_slice_(token_slice) {}

 private:
  void OnSourceElementStart(const raw::SourceElement& el) final { OnToken(&el.start()); }
  void OnSourceElementEnd(const raw::SourceElement& el) final { OnToken(&el.end()); }
  void OnToken(const Token* token);

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

    // TODO(fxbug.dev/118371): This is a hack to work around the limitations of |TokenIterator|
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
        reporter_was_already_silenced_(reporter->ignore_fixables()) {
    // No reporting of other fixable errors - we don't want one fixable error to derail to fixing of
    // another one.
    reporter_->set_ignore_fixables(true);
  }
  ~Transformer() override {
    if (!reporter_was_already_silenced_) {
      reporter_->set_ignore_fixables(false);
    }
  }

  // Handles preparation (input parsing, map building, etc) for the specified transformer
  // derivation. If any of these steps fail, it means that the inputs were incorrect (missing files,
  // parse errors, etc).
  virtual bool Prepare() = 0;

  // Performs the actual transformations, modifying the each raw AST + token list pair in sync to
  // properly reflect the new, post-transformation state. Must be followed by a call to |Format| to
  // take the now transformed data and re-print it back into source strings.
  bool Transform();

  // After transforming successfully, we want to loop back through and format each transformed file.
  // If there is an error here (ie, at least one of the files fails to format), we know that there
  // was very likely some error in the transformation logic (or, less likely, the formatter itself
  // is broken).
  //
  // The formatted file strings are returned in the same order that the original |SourceFiles| were
  // supplied to this class.
  std::optional<std::vector<std::string>> Format();

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
  void AddError(const std::string& msg);

  // This operation readies the passed in source files for transformation. Specifically, it parses
  // each of them, and returns false if any (non-fixable) errors were reported during that process,
  // indicating that the files have problems that need to be fixed before they can be run through
  // a transformer.
  //
  // For each entry in the given |source_files| vector, create a corresponding |FileTransformState|
  // to track transformation progress as we iterate over files. This will require parsing the source
  // file twice: once to create an immutable raw AST to walk over, and a second time to make a
  // mutable copy to transform.
  bool BuildTransformStates();
  FileTransformState& CurrentlyTransforming() { return transform_states_[index_]; }

  // Get the mutable copy of the raw AST node currently being visited. The supplied |el| argument
  // must be a pointer into the existing, immutable raw AST, not the mutable one.
  raw::SourceElement* GetAsMutable(const raw::SourceElement* el);

  // Create a |TokenSlice|. Note that the start and end |Token| passed to this function are
  // inclusive.
  //
  // TODO(fxbug.dev/118371): There is certainly a way to make this faster by not searching the
  // entire |MutableTokenPointerList| every time we call this, and instead keeping some state
  // tracking what range in the list we are looking at for each |SourceElement|, but this is an
  // optimization for later.
  TokenSlice GetTokenSlice(Token from, Token until);

  // Must be called before every |When*| method call, so that any user modifications to the raw AST
  // are "resolved" properly (see the comment on |TokenPointerResolver| for more information).
  static TokenPointerResolver GetTokenPointerResolver(TokenSlice* token_slice);
  void NextStep();

  // Parse a source file. A return value of |nullptr| means that the parse failed, and that all the
  // interesting details will be found in the |reporter|.
  static std::unique_ptr<raw::File> ParseSource(const SourceFile* source_file,
                                                const fidl::ExperimentalFlags& experimental_flags,
                                                Reporter* reporter);

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

  // Finds a specific |Token| of interest in a |MutableTokenPointerList|, given a specific start
  // point. A typed convenience wrapper around |std::find|.
  std::optional<TokenIterator> FindIn(TokenIterator begin, TokenIterator end, Token needle) {
    TokenIterator matched =
        std::find_if(begin, end, [&needle](const Token* entry) { return *entry == needle; });
    if (matched == end) {
      return std::nullopt;
    }
    return std::make_optional<TokenIterator>(matched);
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

}  // namespace internal

// A |Transformer| that transforms a parsed raw AST, with no compilation (aka flat AST) information
// available. Files passed to this class do not necessarily need to compile - they just need to
// parse successfully. See the description on |Transformer| for more information.
//
// Implementors are expected to use this class by deriving it to make their own |*Transformer|,
// with the appropriate |When*| methods overwritten.
class ParsedTransformer : public internal::Transformer {
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
  virtual void WhenOverlayDeclaration(raw::Layout*, TokenSlice&) {}
  virtual void WhenOverlayMember(raw::OrdinaledLayoutMember*, TokenSlice&) {}

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
class CompiledTransformer : public internal::Transformer {
 public:
  CompiledTransformer(const std::vector<const SourceFile*>& library_source_files,
                      const std::vector<std::vector<const SourceFile*>>& dependencies_source_files,
                      const fidl::VersionSelection* version_selection,
                      const fidl::ExperimentalFlags& experimental_flags, Reporter* reporter)
      : Transformer(library_source_files, experimental_flags, reporter),
        version_selection_(version_selection),
        virtual_file_("generated"),
        all_libraries_(reporter, &virtual_file_),
        dependencies_source_files_(dependencies_source_files) {}
  CompiledTransformer(const std::vector<const SourceFile*>& library_source_files,
                      const fidl::VersionSelection* version_selection,
                      const fidl::ExperimentalFlags& experimental_flags, Reporter* reporter)
      : CompiledTransformer(library_source_files, {}, version_selection, experimental_flags,
                            reporter) {}

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
  virtual void WhenOverlayDeclaration(raw::Layout*, TokenSlice&,
                                      const VersionedEntry<flat::Overlay>*) {}
  virtual void WhenOverlayMember(raw::OrdinaledLayoutMember*, TokenSlice&,
                                 const VersionedEntry<flat::Overlay::Member>*) {}

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
  const fidl::VersionSelection* version_selection_;
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
