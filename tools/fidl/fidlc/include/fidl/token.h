// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef TOOLS_FIDL_FIDLC_INCLUDE_FIDL_TOKEN_H_
#define TOOLS_FIDL_FIDLC_INCLUDE_FIDL_TOKEN_H_

#include <stdint.h>

#include <string_view>

#include "tools/fidl/fidlc/include/fidl/source_span.h"

namespace fidl {

class SyntheticToken;

// A Token represents a typed view into a source buffer. That is, it
// has a TokenKind, and it has a buffer representing the data
// corresponding to the token. No processing is done on the data:
// string or numeric literals aren't further parsed, identifiers
// uniqued, and so on.
class Token {
 public:
  enum Kind : uint8_t {
#define TOKEN(Name) k##Name,
#include "tools/fidl/fidlc/include/fidl/token_definitions.inc"
#undef TOKEN
  };

  // Each identifier token is assigned a subkind, which are defined below, or a sentinel value if
  // the token does not match one of the subkinds, to make it simple for the parser to either
  // consume a generic identifier or one that matches a specific subkind. This design makes it so
  // that there are no "reserved keywords" and any identifier can be defined by the user.
  enum Subkind : uint8_t {
    kNone = 0,
#define TOKEN_SUBKIND(Name, Spelling) k##Name,
#include "tools/fidl/fidlc/include/fidl/token_definitions.inc"
#undef TOKEN_SUBKIND
  };

  class KindAndSubkind {
   public:
    constexpr KindAndSubkind(Kind kind, Subkind subkind) : kind_(kind), subkind_(subkind) {}

    constexpr Kind kind() const { return kind_; }
    constexpr Subkind subkind() const { return subkind_; }
    constexpr uint16_t combined() const {
      return static_cast<uint16_t>(kind_) | static_cast<uint16_t>(subkind_ << 8);
    }

   private:
    Kind kind_;
    Subkind subkind_;
  };

  Token(SourceSpan previous_end, SourceSpan span, uint16_t leading_newlines, Kind kind,
        Subkind subkind, uint32_t ordinal)
      : previous_end_(previous_end),
        span_(span),
        leading_newlines_(leading_newlines),
        kind_and_subkind_(KindAndSubkind(kind, subkind)),
        ordinal_(ordinal) {}

  Token()
      : Token(SourceSpan(), SourceSpan(), 0, Token::Kind::kNotAToken, Token::Subkind::kNone, 0) {}

  static const char* Name(KindAndSubkind kind_and_subkind) {
    switch (kind_and_subkind.combined()) {
#define TOKEN(Name)          \
  case Token::Kind::k##Name: \
    return #Name;
#include "tools/fidl/fidlc/include/fidl/token_definitions.inc"
#undef TOKEN
#define TOKEN_SUBKIND(Name, Spelling)                                                       \
  case Token::KindAndSubkind(Token::Kind::kIdentifier, Token::Subkind::k##Name).combined(): \
    return #Spelling;
#include "tools/fidl/fidlc/include/fidl/token_definitions.inc"
#undef TOKEN_SUBKIND
      default:
        return "<unknown token>";
    }
  }

  std::string_view data() const { return span_.data(); }
  const SourceSpan& span() const { return span_; }
  uint16_t leading_newlines() const { return leading_newlines_; }

  // TODO(fxbug.dev/117642): We should remove this, and the underlying member it exposes,
  // altogether. The only major use (which may actually be dead code) is in the linter.
  SourceSpan previous_end() const { return previous_end_; }
  Kind kind() const { return kind_and_subkind_.kind(); }
  Subkind subkind() const { return kind_and_subkind_.subkind(); }
  KindAndSubkind kind_and_subkind() const { return kind_and_subkind_; }
  uint32_t ordinal() const { return ordinal_; }

  void set_leading_newlines(uint16_t leading_newlines) { leading_newlines_ = leading_newlines; }
  void set_previous_end(SourceSpan span) { previous_end_ = span; }
  void set_ordinal(uint32_t ordinal) { ordinal_ = ordinal; }
  uint32_t sub_ordinal() const { return sub_ordinal_; }

  bool same_file_as(const Token& rhs) const {
    return span_.source_file().filename() == rhs.span().source_file().filename();
  }

  bool is_synthetic() const { return sub_ordinal_ != 0; }

  constexpr bool operator==(const Token& rhs) const { return this->span() == rhs.span(); }
  constexpr bool operator!=(const Token& rhs) const { return !(*this == rhs); }

  // Files are sorted alphabetically for the purpose of ordering. Generally speaking, however,
  // tokens (synthetic or sourced) that do not represent spans in the same source file should not be
  // compared against one another. The |same_file_as()| method may be used first to ensure that this
  // is the case.
  constexpr bool operator<(const Token& rhs) const {
    if (this->same_file_as(rhs)) {
      if (ordinal_ == rhs.ordinal_) {
        return sub_ordinal_ < rhs.sub_ordinal_;
      }
      return ordinal_ < rhs.ordinal_;
    }
    return span_.source_file().filename() < rhs.span_.source_file().filename();
  }
  constexpr bool operator>=(const Token& rhs) const { return !(*this < rhs); }
  constexpr bool operator>(const Token& rhs) const { return !(rhs >= *this); }
  constexpr bool operator<=(const Token& rhs) const { return !(*this > rhs); }

 private:
  // The end of the previous token.  Everything between this and span_ is
  // somehow uninteresting to the parser (whitespace, comments, discarded
  // braces, etc).
  SourceSpan previous_end_;
  SourceSpan span_;
  uint16_t leading_newlines_;
  KindAndSubkind kind_and_subkind_;
  uint32_t ordinal_;

 protected:
  // The zero |sub_ordinal_| is used to indicate that |Token| is sourced - that is, it came from a
  // static buffer of FIDL content, and was not created and inserted into the raw AST later by a
  // |Transformer|.
  //
  // When ordering |Token|s, we first compare |ordinal_|s. If |ordinal_|s are equal, at least one of
  // the |Token|s in question is a |SyntheticToken|, denoted by having a |sub_ordinal_| greater than
  // 0. Ordering between such |Token|s is determined by comparing this |sub_ordinal_| value.
  //
  // Because every file must start with a non-synthetic |Token::Kind::kStartOfFile| and end with
  // with a |Token::Kind::kEndOfFile| and those |Token|s must not be altered or replaced, we can be
  // sure that every |SyntheticToken| is a "child" of some original, sourced |Token|. To maintain
  // this invariant, care should be taken when removing an existing |Token| from a token list, since
  // all of its attached |SyntheticToken|s must be assigned to its predecessor instead.
  uint32_t sub_ordinal_ = 0;
};

// A |Token| that has been generated post-parsing, and injected into a raw AST tree and/or
// |TokenPointerList|. Unlike it's surrounding |Token|s, this |Token| does NOT come from the
// original FIDL buffer used during the first lexing and parsing pass. This means that it cannot be
// positioned relative to other |Token|s via pointer arithmetic alone, and must instead rely on
// having its |ordinal_| and |sub_ordinal_| correctly assigned. Every |SyntheticToken| is thus a
// "child" of some original |Token|, in that it shares an |ordinal_| with it, and appears directly
// after it in the token list.
class SyntheticToken : public Token {
 public:
  SyntheticToken(SourceSpan previous_end, SourceSpan span, uint16_t leading_newlines,
                 Token::Kind kind, Token::Subkind subkind, uint32_t ordinal, uint32_t sub_ordinal)
      : Token(previous_end, span, leading_newlines, kind, subkind, ordinal) {
    ZX_ASSERT(sub_ordinal > 0);
    sub_ordinal_ = sub_ordinal;
  }

  void set_sub_ordinal(uint32_t sub_ordinal) { sub_ordinal_ = sub_ordinal; }
};

}  // namespace fidl

#endif  // TOOLS_FIDL_FIDLC_INCLUDE_FIDL_TOKEN_H_
