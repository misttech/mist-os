// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef TOOLS_FIDL_FIDLC_INCLUDE_FIDL_RAW_AST_H_
#define TOOLS_FIDL_FIDLC_INCLUDE_FIDL_RAW_AST_H_

#include <zircon/assert.h>

#include <memory>
#include <optional>
#include <string>
#include <utility>
#include <variant>
#include <vector>

#include "tools/fidl/fidlc/include/fidl/source_span.h"
#include "tools/fidl/fidlc/include/fidl/token.h"
#include "tools/fidl/fidlc/include/fidl/types.h"
#include "tools/fidl/fidlc/include/fidl/utils.h"

// ASTs fresh out of the oven. This is a tree-shaped bunch of nodes
// pretty much exactly corresponding to the grammar of a single fidl
// file. File is the root of the tree, and consists of lists of
// Declarations, and so on down to individual SourceSpans.
// See
// https://fuchsia.dev/fuchsia-src/development/languages/fidl/reference/compiler#compiler_internals
// for additional context

// Each node owns its children via unique_ptr and vector. All tokens
// here, like everywhere in the fidl compiler, are backed by a string
// view whose contents are owned by a SourceManager.

// This class has a tight coupling with the TreeVisitor class.  Each node has a
// corresponding method in that class.  Each node type also has an Accept()
// method to help visitors visit the node.  When you add a new node, or add a
// field to an existing node, you must ensure the Accept method works.

// A raw::File is produced by parsing a token stream. All of the
// Files in a library are then flattened out into a Library.

namespace fidl::raw {

// In order to be able to associate AST nodes with their original source, each
// node is a SourceElement, which contains information about the original
// source.  The AST has a start token and an end token, which point to the start
// and end of this syntactic element, respectively.
class TreeVisitor;

// A collection of one or more consecutive |Token|s from a single |SourceFile|.
class TokenChain {
 public:
  TokenChain(Token start, Token end) : start_(start), end_(end) {}
  virtual ~TokenChain() = default;

  bool has_span() const {
    return start_.span().valid() && end_.span().valid() &&
           &start_.span().source_file() == &end_.span().source_file();
  }

  // This method should not be called on post-transform |TokenChain|s, as it necessarily
  // pre-supposes that all of the the returned |SourceSpan| is still a valid view into the source,
  // which may no longer be the case. In particular, the formatter should avoid using this method.
  //
  // TODO(fxbug.dev/118371): Find a better way than the above comment to prevent the post-transform
  // context like the formatter from trying to use this method. This will likely involve specifying
  // whether or not the underlying |SourceFile| is "mutable", and placing an assert inside this
  // method to prevent |span()| calls on mutable |SourceFile|s.
  SourceSpan span() const {
    if (!start_.span().valid() || !end_.span().valid()) {
      return SourceSpan();
    }

    ZX_ASSERT(has_span());
    const char* start_pos = start_.span().data().data();
    const char* end_pos = end_.span().data().data() + end_.span().data().length();
    return SourceSpan(std::string_view(start_pos, end_pos - start_pos),
                      start_.span().source_file());
  }

  void update_span(const TokenChain& element) {
    start_ = element.start_;
    end_ = element.end_;
  }

  void set_start(Token token) { start_ = token; }

  Token& start() { return start_; }

  const Token& start() const { return start_; }

  void set_end(Token token) { end_ = token; }

  Token& end() { return end_; }

  const Token& end() const { return end_; }

 private:
  Token start_;
  Token end_;
};

// A collection of one or more consecutive |Token|s from a single |SourceFile|, with a |NodeKind|
// attached. All other nodes in the raw abstract syntax tree inherit from this class.
class SourceElement : public TokenChain {
 public:
  enum NodeKind {
    kAliasDeclaration,
    kAttribute,
    kAttributeArg,
    kAttributeList,
    kBinaryOperatorConstant,
    kBoolLiteral,
    kCompoundIdentifier,
    kConstDeclaration,
    kDocCommentLiteral,
    kFile,
    kIdentifier,
    kIdentifierConstant,
    kIdentifierLayoutParameter,
    kInlineLayoutReference,
    kLayoutParameterList,
    kLibraryDeclaration,
    kLiteralConstant,
    kLiteralLayoutParameter,
    kModifiers,
    kNamedLayoutReference,
    kNumericLiteral,
    kOrdinal64,
    kOrdinaledLayout,
    kOrdinaledLayoutMember,
    kParameterList,
    kProtocolCompose,
    kProtocolDeclaration,
    kProtocolMethod,
    kResourceDeclaration,
    kResourceProperty,
    kServiceDeclaration,
    kServiceMember,
    kStringLiteral,
    kStructLayout,
    kStructLayoutMember,
    kTypeConstraints,
    kTypeConstructor,
    kTypeDeclaration,
    kTypeLayoutParameter,
    kUsing,
    kValueLayout,
    kValueLayoutMember,
  };

  SourceElement(NodeKind node_kind, Token start, Token end)
      : TokenChain(start, end), node_kind_(node_kind) {}
  SourceElement(const TokenChain& element, NodeKind node_kind)
      : TokenChain(element), node_kind_(node_kind) {}

  NodeKind node_kind() const { return node_kind_; }

  NodeKind node_kind_;

  // Represents source span in a FIDL source file, without revealing the contents of that range.
  // This is meant to serve as a lightweight unique identifier of some |SourceSpan| of interest. The
  // class is intended to be attached to a flat AST node, so that we may later compare that node
  // against raw AST ranges to ascertain where it was sourced from.
  //
  // The lifetime of this |Signature| is only as long as that of the |SourceFile| it is a view into.
  // Importantly, comparing identical segments of two different |SourceFile|s will produce diverging
  // |Signature|s. This means that the |Signature| is only useful when comparing multiple segments
  // referencing into the same |SourceFile| instance in memory.
  class Signature {
   public:
    constexpr Signature(NodeKind node_kind, std::string_view data)
        : node_kind_(node_kind), data_(data) {}

    constexpr Signature(const Signature&) = default;
    constexpr Signature(Signature&&) = default;
    constexpr Signature& operator=(const Signature&) = default;
    constexpr Signature& operator=(Signature&&) = default;

    constexpr bool operator==(const Signature& rhs) const {
      return node_kind_ == rhs.node_kind_ && data_.data() == rhs.data_.data() &&
             data_.size() == data_.size();
    }
    constexpr bool operator<(const Signature& rhs) const {
      // Assumes both |Signature|s are views into the same buffer.
      if (this->data_.data() != rhs.data_.data()) {
        return this->data_.data() < rhs.data_.data();
      }

      // If two |Signature|s have identical start points, compare the end points instead.
      if (this->data_.size() != rhs.data_.size()) {
        return this->data_.size() < rhs.data_.size();
      }

      // Only sort by |NodeKind| if the underlying string segment is identical. This sorting is a
      // bit arbitrary, but is currently only used by test code.
      return node_kind_ < rhs.node_kind_;
    }

   private:
    friend struct std::hash<Signature>;

    // Use |NodeKind| to discriminate between raw AST nodes that may share the same underlying
    // source string, like the marked |raw::TypeLayoutParameter|, |raw::TypeConstructor|, and
    // |raw::Layout| in the example below:
    //
    //   type Foo = vector<struct{}>;
    //                     ^^^^^^^^
    //
    NodeKind node_kind_;

    // It is important that this remain private. While simply forwarding the |string_view| provides
    // an easy way to uniquely identify a source segment, it can also be tempting to "hack" access
    // to the source from the flat AST through this field. Resist this temptation! Any raw AST data
    // used by the compiler should be parsed into the raw AST, consumed in the consumer step, etc,
    // rather than pulled from this string later in compilation.
    std::string_view data_;
  };

  Signature source_signature() const { return Signature(node_kind_, span().data()); }
};

class SourceElementMark {
 public:
  SourceElementMark(TreeVisitor* tv, const SourceElement& element);

  ~SourceElementMark();

 private:
  TreeVisitor* tv_;
  const SourceElement& element_;
};

class Identifier final : public SourceElement {
 public:
  explicit Identifier(const TokenChain& element) : SourceElement(element, kIdentifier) {}

  void Accept(TreeVisitor* visitor) const;
};

class CompoundIdentifier final : public SourceElement {
 public:
  CompoundIdentifier(const TokenChain& element, std::vector<std::unique_ptr<Identifier>> components)
      : SourceElement(element, kCompoundIdentifier), components(std::move(components)) {}

  void Accept(TreeVisitor* visitor) const;

  std::vector<std::unique_ptr<Identifier>> components;
};

class Literal : public SourceElement {
 public:
  enum struct Kind {
    kBool,
    kDocComment,
    kNumeric,
    kString,
  };

  explicit Literal(const TokenChain& element, Kind kind)
      : SourceElement(element, NodeKind(kind)), kind(kind) {}

  static SourceElement::NodeKind NodeKind(Kind kind) {
    switch (kind) {
      case Kind::kBool:
        return SourceElement::NodeKind::kBoolLiteral;
      case Kind::kDocComment:
        return SourceElement::NodeKind::kDocCommentLiteral;
      case Kind::kNumeric:
        return SourceElement::NodeKind::kNumericLiteral;
      case Kind::kString:
        return SourceElement::NodeKind::kStringLiteral;
    }
  }

  const Kind kind;
};

class DocCommentLiteral final : public Literal {
 public:
  explicit DocCommentLiteral(const TokenChain& element) : Literal(element, Kind::kDocComment) {}

  void Accept(TreeVisitor* visitor) const;

  std::string MakeContents() const {
    if (!has_span() || span().data().empty()) {
      return "";
    }
    return fidl::utils::strip_doc_comment_slashes(span().data());
  }
};

class StringLiteral final : public Literal {
 public:
  explicit StringLiteral(const TokenChain& element) : Literal(element, Kind::kString) {}

  void Accept(TreeVisitor* visitor) const;

  std::string MakeContents() const {
    if (!has_span() || span().data().empty()) {
      return "";
    }
    return fidl::utils::strip_string_literal_quotes(span().data());
  }
};

class NumericLiteral final : public Literal {
 public:
  explicit NumericLiteral(const TokenChain& element) : Literal(element, Kind::kNumeric) {}

  void Accept(TreeVisitor* visitor) const;
};

class BoolLiteral final : public Literal {
 public:
  BoolLiteral(const TokenChain& element, bool value)
      : Literal(element, Kind::kBool), value(value) {}

  void Accept(TreeVisitor* visitor) const;

  const bool value;
};

class Ordinal64 final : public SourceElement {
 public:
  Ordinal64(const TokenChain& element, uint64_t value)
      : SourceElement(element, kOrdinal64), value(value) {}

  void Accept(TreeVisitor* visitor) const;

  const uint64_t value;
};

class Constant : public SourceElement {
 public:
  enum class Kind { kIdentifier, kLiteral, kBinaryOperator };

  explicit Constant(Token start, Token end, Kind kind)
      : SourceElement(TokenChain(start, end), NodeKind(kind)), kind(kind) {}
  explicit Constant(const TokenChain& element, Kind kind)
      : SourceElement(element, NodeKind(kind)), kind(kind) {}

  static SourceElement::NodeKind NodeKind(Kind kind) {
    switch (kind) {
      case Kind::kBinaryOperator:
        return SourceElement::NodeKind::kBinaryOperatorConstant;
      case Kind::kIdentifier:
        return SourceElement::NodeKind::kIdentifierConstant;
      case Kind::kLiteral:
        return SourceElement::NodeKind::kLiteralConstant;
    }
  }

  const Kind kind;
};

class IdentifierConstant final : public Constant {
 public:
  explicit IdentifierConstant(std::unique_ptr<CompoundIdentifier> identifier)
      : Constant(SourceElement(kIdentifierConstant, identifier->start(), identifier->end()),
                 Kind::kIdentifier),
        identifier(std::move(identifier)) {}

  std::unique_ptr<CompoundIdentifier> identifier;

  void Accept(TreeVisitor* visitor) const;
};

class LiteralConstant final : public Constant {
 public:
  explicit LiteralConstant(std::unique_ptr<Literal> literal)
      : Constant(literal->start(), literal->end(), Kind::kLiteral), literal(std::move(literal)) {}

  std::unique_ptr<Literal> literal;

  void Accept(TreeVisitor* visitor) const;
};

class BinaryOperatorConstant final : public Constant {
 public:
  enum class Operator { kOr };
  explicit BinaryOperatorConstant(std::unique_ptr<Constant> left_operand,
                                  std::unique_ptr<Constant> right_operand, Operator op)
      : Constant(
            SourceElement(kBinaryOperatorConstant, left_operand->start(), right_operand->end()),
            Kind::kBinaryOperator),
        left_operand(std::move(left_operand)),
        right_operand(std::move(right_operand)),
        op(op) {}

  std::unique_ptr<Constant> left_operand;
  std::unique_ptr<Constant> right_operand;
  Operator op;

  void Accept(TreeVisitor* visitor) const;
};

class AttributeArg final : public SourceElement {
 public:
  // Constructor for cases where the arg name has been explicitly defined in the text.
  AttributeArg(const TokenChain& element, std::unique_ptr<Identifier> name,
               std::unique_ptr<Constant> value)
      : SourceElement(element, kAttributeArg),
        maybe_name(std::move(name)),
        value(std::move(value)) {}

  // Constructor for cases where the arg name is inferred.
  AttributeArg(const TokenChain& element, std::unique_ptr<Constant> value)
      : SourceElement(element, kAttributeArg), maybe_name(nullptr), value(std::move(value)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<Identifier> maybe_name;
  std::unique_ptr<Constant> value;
};

class Attribute final : public SourceElement {
 public:
  enum Provenance {
    kDefault,
    kDocComment,
  };

  // Constructor for cases where the name of the attribute is explicitly defined in the text.
  Attribute(const TokenChain& element, std::unique_ptr<Identifier> maybe_name,
            std::vector<std::unique_ptr<AttributeArg>> args)
      : SourceElement(element, kAttribute),
        maybe_name(std::move(maybe_name)),
        args(std::move(args)) {}

  // Factory for "///"-style doc comments, which have no attribute name.
  static Attribute CreateDocComment(const TokenChain& element,
                                    std::vector<std::unique_ptr<AttributeArg>> args) {
    auto attr = Attribute(element, nullptr, std::move(args));
    attr.provenance = kDocComment;
    return attr;
  }

  void Accept(TreeVisitor* visitor) const;

  Provenance provenance = Provenance::kDefault;
  std::unique_ptr<Identifier> maybe_name;
  std::vector<std::unique_ptr<AttributeArg>> args;
};

// In the raw AST, "no attributes" is represented by a null AttributeList*,
// because every SourceElement must have a valid span. (In the flat AST, it is
// the opposite: never null, but the vector can be empty.)
class AttributeList final : public SourceElement {
 public:
  AttributeList(const TokenChain& element, std::vector<std::unique_ptr<Attribute>> attributes)
      : SourceElement(element, kAttributeList), attributes(std::move(attributes)) {}

  void Accept(TreeVisitor* visitor) const;

  std::vector<std::unique_ptr<Attribute>> attributes;
};

class TypeConstructor;

class LayoutReference;
class LayoutParameterList;
class TypeConstraints;

// The monostate variant is used to represent a parse failure.
using ConstraintOrSubtype = std::variant<std::unique_ptr<TypeConstraints>,
                                         std::unique_ptr<TypeConstructor>, std::monostate>;

class TypeConstructor final : public SourceElement {
 public:
  TypeConstructor(const TokenChain& element, std::unique_ptr<LayoutReference> layout_ref,
                  std::unique_ptr<LayoutParameterList> parameters,
                  std::unique_ptr<TypeConstraints> constraints)
      : SourceElement(element, kTypeConstructor),
        layout_ref(std::move(layout_ref)),
        parameters(std::move(parameters)),
        constraints(std::move(constraints)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<LayoutReference> layout_ref;
  std::unique_ptr<LayoutParameterList> parameters;
  std::unique_ptr<TypeConstraints> constraints;
};

class AliasDeclaration final : public SourceElement {
 public:
  AliasDeclaration(const TokenChain& element, std::unique_ptr<AttributeList> attributes,
                   std::unique_ptr<Identifier> alias, std::unique_ptr<TypeConstructor> type_ctor)
      : SourceElement(element, kAliasDeclaration),
        attributes(std::move(attributes)),
        alias(std::move(alias)),
        type_ctor(std::move(type_ctor)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<AttributeList> attributes;
  std::unique_ptr<Identifier> alias;
  std::unique_ptr<TypeConstructor> type_ctor;
};

class LibraryDeclaration final : public SourceElement {
 public:
  LibraryDeclaration(const TokenChain& element, std::unique_ptr<AttributeList> attributes,
                     std::unique_ptr<CompoundIdentifier> path)
      : SourceElement(element, kLibraryDeclaration),
        attributes(std::move(attributes)),
        path(std::move(path)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<AttributeList> attributes;
  std::unique_ptr<CompoundIdentifier> path;
};

class Using final : public SourceElement {
 public:
  Using(const TokenChain& element, std::unique_ptr<AttributeList> attributes,
        std::unique_ptr<CompoundIdentifier> using_path, std::unique_ptr<Identifier> maybe_alias)
      : SourceElement(element, kUsing),
        attributes(std::move(attributes)),
        using_path(std::move(using_path)),
        maybe_alias(std::move(maybe_alias)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<AttributeList> attributes;
  std::unique_ptr<CompoundIdentifier> using_path;
  std::unique_ptr<Identifier> maybe_alias;
};

class ConstDeclaration final : public SourceElement {
 public:
  ConstDeclaration(const TokenChain& element, std::unique_ptr<AttributeList> attributes,
                   std::unique_ptr<TypeConstructor> type_ctor,
                   std::unique_ptr<Identifier> identifier, std::unique_ptr<Constant> constant)
      : SourceElement(element, kConstDeclaration),
        attributes(std::move(attributes)),
        type_ctor(std::move(type_ctor)),
        identifier(std::move(identifier)),
        constant(std::move(constant)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<AttributeList> attributes;
  std::unique_ptr<TypeConstructor> type_ctor;
  std::unique_ptr<Identifier> identifier;
  std::unique_ptr<Constant> constant;
};

// A single modifier applied to a layout, protocol, or method.
template <typename T>
class Modifier final {
 public:
  Modifier(T value, Token token) : value(value), token(token) {}

  // Value of the modifier
  T value;
  // Token that the modifier is from.
  Token token;
};

class Modifiers final : public SourceElement {
 public:
  // Constructor for Layouts (has resourceness and strictness, but not openness).
  Modifiers(const TokenChain& element,
            std::optional<Modifier<types::Resourceness>> maybe_resourceness,
            std::optional<Modifier<types::Strictness>> maybe_strictness,
            bool resourceness_comes_first)
      : SourceElement(element, kModifiers),
        maybe_resourceness(maybe_resourceness),
        maybe_strictness(maybe_strictness),
        maybe_openness(std::nullopt),
        resourceness_comes_first(resourceness_comes_first) {}

  // Constructor for Protocols (only has openness).
  Modifiers(const TokenChain& element, std::optional<Modifier<types::Openness>> maybe_openness)
      : SourceElement(element, kModifiers),
        maybe_resourceness(std::nullopt),
        maybe_strictness(std::nullopt),
        maybe_openness(maybe_openness),
        resourceness_comes_first(false) {}

  // Constructor for Protocol methods (only has strictness).
  Modifiers(const TokenChain& element, std::optional<Modifier<types::Strictness>> maybe_strictness)
      : SourceElement(element, kModifiers),
        maybe_resourceness(std::nullopt),
        maybe_strictness(maybe_strictness),
        maybe_openness(std::nullopt),
        resourceness_comes_first(false) {}

  void Accept(TreeVisitor* visitor) const;

  std::optional<Modifier<types::Resourceness>> maybe_resourceness;
  std::optional<Modifier<types::Strictness>> maybe_strictness;
  std::optional<Modifier<types::Openness>> maybe_openness;
  // Whether the resourceness modifier for a layout was before the strictness
  // modifier, used for linting.
  bool resourceness_comes_first;
};

class ParameterList final : public SourceElement {
 public:
  ParameterList(const TokenChain& element, std::unique_ptr<TypeConstructor> type_ctor)
      : SourceElement(element, kParameterList), type_ctor(std::move(type_ctor)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<TypeConstructor> type_ctor;
};

class ProtocolMethod : public SourceElement {
 public:
  ProtocolMethod(const TokenChain& element, std::unique_ptr<AttributeList> attributes,
                 std::unique_ptr<Modifiers> modifiers, std::unique_ptr<Identifier> identifier,
                 std::unique_ptr<ParameterList> maybe_request,
                 std::unique_ptr<ParameterList> maybe_response,
                 std::unique_ptr<TypeConstructor> maybe_error_ctor)
      : SourceElement(element, kProtocolMethod),
        attributes(std::move(attributes)),
        modifiers(std::move(modifiers)),
        identifier(std::move(identifier)),
        maybe_request(std::move(maybe_request)),
        maybe_response(std::move(maybe_response)),
        maybe_error_ctor(std::move(maybe_error_ctor)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<AttributeList> attributes;
  std::unique_ptr<Modifiers> modifiers;
  std::unique_ptr<Identifier> identifier;
  std::unique_ptr<ParameterList> maybe_request;
  std::unique_ptr<ParameterList> maybe_response;
  std::unique_ptr<TypeConstructor> maybe_error_ctor;
};

class ProtocolCompose final : public SourceElement {
 public:
  ProtocolCompose(const TokenChain& element, std::unique_ptr<AttributeList> attributes,
                  std::unique_ptr<CompoundIdentifier> protocol_name)
      : SourceElement(element, kProtocolCompose),
        attributes(std::move(attributes)),
        protocol_name(std::move(protocol_name)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<AttributeList> attributes;
  std::unique_ptr<CompoundIdentifier> protocol_name;
};

class ProtocolDeclaration final : public SourceElement {
 public:
  ProtocolDeclaration(const TokenChain& element, std::unique_ptr<AttributeList> attributes,
                      std::unique_ptr<Modifiers> modifiers, std::unique_ptr<Identifier> identifier,
                      std::vector<std::unique_ptr<ProtocolCompose>> composed_protocols,
                      std::vector<std::unique_ptr<ProtocolMethod>> methods)
      : SourceElement(element, kProtocolDeclaration),
        attributes(std::move(attributes)),
        modifiers(std::move(modifiers)),
        identifier(std::move(identifier)),
        composed_protocols(std::move(composed_protocols)),
        methods(std::move(methods)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<AttributeList> attributes;
  std::unique_ptr<Modifiers> modifiers;
  std::unique_ptr<Identifier> identifier;
  std::vector<std::unique_ptr<ProtocolCompose>> composed_protocols;
  std::vector<std::unique_ptr<ProtocolMethod>> methods;
};

class ResourceProperty final : public SourceElement {
 public:
  ResourceProperty(const TokenChain& element, std::unique_ptr<TypeConstructor> type_ctor,
                   std::unique_ptr<Identifier> identifier,
                   std::unique_ptr<AttributeList> attributes)
      : SourceElement(element, kResourceProperty),
        type_ctor(std::move(type_ctor)),
        identifier(std::move(identifier)),
        attributes(std::move(attributes)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<TypeConstructor> type_ctor;
  std::unique_ptr<Identifier> identifier;
  std::unique_ptr<AttributeList> attributes;
};

class ResourceDeclaration final : public SourceElement {
 public:
  ResourceDeclaration(const TokenChain& element, std::unique_ptr<AttributeList> attributes,
                      std::unique_ptr<Identifier> identifier,
                      std::unique_ptr<TypeConstructor> maybe_type_ctor,
                      std::vector<std::unique_ptr<ResourceProperty>> properties)
      : SourceElement(element, kResourceDeclaration),
        attributes(std::move(attributes)),
        identifier(std::move(identifier)),
        maybe_type_ctor(std::move(maybe_type_ctor)),
        properties(std::move(properties)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<AttributeList> attributes;
  std::unique_ptr<Identifier> identifier;
  std::unique_ptr<TypeConstructor> maybe_type_ctor;
  std::vector<std::unique_ptr<ResourceProperty>> properties;
};

class ServiceMember final : public SourceElement {
 public:
  ServiceMember(const TokenChain& element, std::unique_ptr<TypeConstructor> type_ctor,
                std::unique_ptr<Identifier> identifier, std::unique_ptr<AttributeList> attributes)
      : SourceElement(element, kServiceMember),
        type_ctor(std::move(type_ctor)),
        identifier(std::move(identifier)),
        attributes(std::move(attributes)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<TypeConstructor> type_ctor;
  std::unique_ptr<Identifier> identifier;
  std::unique_ptr<AttributeList> attributes;
};

class ServiceDeclaration final : public SourceElement {
 public:
  ServiceDeclaration(const TokenChain& element, std::unique_ptr<AttributeList> attributes,
                     std::unique_ptr<Identifier> identifier,
                     std::vector<std::unique_ptr<ServiceMember>> members)
      : SourceElement(element, kServiceDeclaration),
        attributes(std::move(attributes)),
        identifier(std::move(identifier)),
        members(std::move(members)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<AttributeList> attributes;
  std::unique_ptr<Identifier> identifier;
  std::vector<std::unique_ptr<ServiceMember>> members;
};

// |LayoutMember| is a child(ren) of |Layout|, but |LayoutMember| itself relies on |Layout::Kind|,
// which is defined inside of |Layout|. Forward declare to break this cycle.
class LayoutMember;

class Layout final : public SourceElement {
 public:
  enum Kind {
    kBits,
    kEnum,
    kStruct,
    kTable,
    kUnion,
    kOverlay,
  };

  Layout(const TokenChain& element, Kind kind, std::vector<std::unique_ptr<LayoutMember>> members,
         std::unique_ptr<Modifiers> modifiers, std::unique_ptr<TypeConstructor> subtype_ctor)
      : SourceElement(element, NodeKind(kind)),
        kind(kind),
        members(std::move(members)),
        modifiers(std::move(modifiers)),
        subtype_ctor(std::move(subtype_ctor)) {}

  void Accept(TreeVisitor* visitor) const;

  static SourceElement::NodeKind NodeKind(Kind kind) {
    switch (kind) {
      case Kind::kBits:
      case Kind::kEnum:
        return SourceElement::NodeKind::kValueLayout;
      case Kind::kStruct:
        return SourceElement::NodeKind::kStructLayout;
      case Kind::kTable:
      case Kind::kUnion:
      case Kind::kOverlay:
        return SourceElement::NodeKind::kOrdinaledLayout;
    }
  }

  Kind kind;
  std::vector<std::unique_ptr<raw::LayoutMember>> members;
  std::unique_ptr<Modifiers> modifiers;
  // TODO(fxbug.dev/77853): Eventually we'll make [Struct/Ordinaled/Value]Layout
  //  classes to inherit from the now-abstract Layout class, similar to what can
  //  currently be seen on LayoutMember and its children.  When that happens
  //  this field will only exist on ValueLayout.
  std::unique_ptr<TypeConstructor> subtype_ctor;
};

class LayoutMember : public SourceElement {
 public:
  enum Kind {
    kOrdinaled,
    kStruct,
    kValue,
  };

  explicit LayoutMember(const TokenChain& element, Kind kind, Layout::Kind layout_kind,
                        std::unique_ptr<AttributeList> attributes,
                        std::unique_ptr<Identifier> identifier)
      : SourceElement(element, NodeKind(kind)),
        kind(kind),
        layout_kind(layout_kind),
        attributes(std::move(attributes)),
        identifier(std::move(identifier)) {}

  void Accept(TreeVisitor* visitor) const;

  static SourceElement::NodeKind NodeKind(Kind kind) {
    switch (kind) {
      case Kind::kOrdinaled:
        return SourceElement::NodeKind::kOrdinaledLayoutMember;
      case Kind::kStruct:
        return SourceElement::NodeKind::kStructLayoutMember;
      case Kind::kValue:
        return SourceElement::NodeKind::kValueLayoutMember;
    }
  }

  const Kind kind;
  const Layout::Kind layout_kind;
  std::unique_ptr<AttributeList> attributes;
  std::unique_ptr<Identifier> identifier;
};

class OrdinaledLayoutMember final : public LayoutMember {
 public:
  explicit OrdinaledLayoutMember(const TokenChain& element, Layout::Kind layout_kind,
                                 std::unique_ptr<AttributeList> attributes,
                                 std::unique_ptr<Ordinal64> ordinal,
                                 std::unique_ptr<Identifier> identifier,
                                 std::unique_ptr<TypeConstructor> type_ctor)
      : LayoutMember(element, Kind::kOrdinaled, layout_kind, std::move(attributes),
                     std::move(identifier)),
        ordinal(std::move(ordinal)),
        type_ctor(std::move(type_ctor)) {}
  explicit OrdinaledLayoutMember(const TokenChain& element, Layout::Kind layout_kind,
                                 std::unique_ptr<AttributeList> attributes,
                                 std::unique_ptr<Ordinal64> ordinal)
      : LayoutMember(element, Kind::kOrdinaled, layout_kind, std::move(attributes), nullptr),
        ordinal(std::move(ordinal)),
        type_ctor(nullptr),
        reserved(true) {
    ZX_ASSERT(layout_kind == Layout::Kind::kTable || layout_kind == Layout::Kind::kUnion ||
              layout_kind == Layout::Kind::kOverlay);
  }

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<Ordinal64> ordinal;
  std::unique_ptr<TypeConstructor> type_ctor;
  const bool reserved = false;
};

class ValueLayoutMember final : public LayoutMember {
 public:
  explicit ValueLayoutMember(const TokenChain& element, Layout::Kind layout_kind,
                             std::unique_ptr<AttributeList> attributes,
                             std::unique_ptr<Identifier> identifier,
                             std::unique_ptr<Constant> value)
      : LayoutMember(element, Kind::kValue, layout_kind, std::move(attributes),
                     std::move(identifier)),
        value(std::move(value)) {
    ZX_ASSERT(layout_kind == Layout::Kind::kBits || layout_kind == Layout::Kind::kEnum);
  }

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<Constant> value;
};

class StructLayoutMember final : public LayoutMember {
 public:
  explicit StructLayoutMember(const TokenChain& element, Layout::Kind layout_kind,
                              std::unique_ptr<AttributeList> attributes,
                              std::unique_ptr<Identifier> identifier,
                              std::unique_ptr<TypeConstructor> type_ctor,
                              std::unique_ptr<Constant> default_value)
      : LayoutMember(element, Kind::kStruct, layout_kind, std::move(attributes),
                     std::move(identifier)),
        type_ctor(std::move(type_ctor)),
        default_value(std::move(default_value)) {
    ZX_ASSERT(layout_kind == Layout::Kind::kStruct);
  }

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<TypeConstructor> type_ctor;
  std::unique_ptr<Constant> default_value;
};

class LayoutReference : public SourceElement {
 public:
  enum Kind {
    kInline,
    kNamed,
  };

  LayoutReference(const TokenChain& element, Kind kind)
      : SourceElement(element, NodeKind(kind)), kind(kind) {}

  void Accept(TreeVisitor* visitor) const;

  static SourceElement::NodeKind NodeKind(Kind kind) {
    switch (kind) {
      case Kind::kInline:
        return SourceElement::NodeKind::kInlineLayoutReference;
      case Kind::kNamed:
        return SourceElement::NodeKind::kNamedLayoutReference;
    }
  }

  const Kind kind;
};

class InlineLayoutReference final : public LayoutReference {
 public:
  explicit InlineLayoutReference(const TokenChain& element,
                                 std::unique_ptr<AttributeList> attributes,
                                 std::unique_ptr<Layout> layout)
      : LayoutReference(element, Kind::kInline),
        attributes(std::move(attributes)),
        layout(std::move(layout)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<AttributeList> attributes;
  std::unique_ptr<Layout> layout;
};

class NamedLayoutReference final : public LayoutReference {
 public:
  explicit NamedLayoutReference(const TokenChain& element,
                                std::unique_ptr<CompoundIdentifier> identifier)
      : LayoutReference(element, Kind::kNamed), identifier(std::move(identifier)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<CompoundIdentifier> identifier;
};

class LayoutParameter : public SourceElement {
 public:
  enum Kind {
    kIdentifier,
    kLiteral,
    kType,
  };

  LayoutParameter(const TokenChain& element, Kind kind)
      : SourceElement(element, NodeKind(kind)), kind(kind) {}

  static SourceElement::NodeKind NodeKind(Kind kind) {
    switch (kind) {
      case Kind::kIdentifier:
        return SourceElement::NodeKind::kIdentifierLayoutParameter;
      case Kind::kLiteral:
        return SourceElement::NodeKind::kLiteralLayoutParameter;
      case Kind::kType:
        return SourceElement::NodeKind::kTypeLayoutParameter;
    }
  }

  void Accept(TreeVisitor* visitor) const;

  const Kind kind;
};

class LiteralLayoutParameter final : public LayoutParameter {
 public:
  explicit LiteralLayoutParameter(const TokenChain& element,
                                  std::unique_ptr<LiteralConstant> literal)
      : LayoutParameter(element, Kind::kLiteral), literal(std::move(literal)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<LiteralConstant> literal;
};

class TypeLayoutParameter final : public LayoutParameter {
 public:
  explicit TypeLayoutParameter(const TokenChain& element,
                               std::unique_ptr<TypeConstructor> type_ctor)
      : LayoutParameter(element, Kind::kType), type_ctor(std::move(type_ctor)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<TypeConstructor> type_ctor;
};

class IdentifierLayoutParameter final : public LayoutParameter {
 public:
  explicit IdentifierLayoutParameter(const TokenChain& element,
                                     std::unique_ptr<CompoundIdentifier> identifier)
      : LayoutParameter(element, Kind::kIdentifier), identifier(std::move(identifier)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<CompoundIdentifier> identifier;
};

class LayoutParameterList final : public SourceElement {
 public:
  LayoutParameterList(const TokenChain& element,
                      std::vector<std::unique_ptr<raw::LayoutParameter>> items)
      : SourceElement(element, kLayoutParameterList), items(std::move(items)) {}

  void Accept(TreeVisitor* visitor) const;

  std::vector<std::unique_ptr<raw::LayoutParameter>> items;
};

class TypeConstraints final : public SourceElement {
 public:
  TypeConstraints(const TokenChain& element, std::vector<std::unique_ptr<raw::Constant>> items)
      : SourceElement(element, kTypeConstraints), items(std::move(items)) {}

  void Accept(TreeVisitor* visitor) const;

  std::vector<std::unique_ptr<raw::Constant>> items;
};

class TypeDeclaration final : public SourceElement {
 public:
  TypeDeclaration(const TokenChain& element, std::unique_ptr<AttributeList> attributes,
                  std::unique_ptr<Identifier> identifier,
                  std::unique_ptr<TypeConstructor> type_ctor)
      : SourceElement(element, kTypeDeclaration),
        attributes(std::move(attributes)),
        identifier(std::move(identifier)),
        type_ctor(std::move(type_ctor)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<AttributeList> attributes;
  std::unique_ptr<Identifier> identifier;
  std::unique_ptr<TypeConstructor> type_ctor;
};

class File final : public SourceElement {
 public:
  File(const TokenChain& element, Token end, std::unique_ptr<LibraryDeclaration> library_decl,
       std::vector<std::unique_ptr<AliasDeclaration>> alias_list,
       std::vector<std::unique_ptr<Using>> using_list,
       std::vector<std::unique_ptr<ConstDeclaration>> const_declaration_list,
       std::vector<std::unique_ptr<ProtocolDeclaration>> protocol_declaration_list,
       std::vector<std::unique_ptr<ResourceDeclaration>> resource_declaration_list,
       std::vector<std::unique_ptr<ServiceDeclaration>> service_declaration_list,
       std::vector<std::unique_ptr<TypeDeclaration>> type_decls, std::vector<Token> tokens)
      : SourceElement(element, kFile),
        library_decl(std::move(library_decl)),
        alias_list(std::move(alias_list)),
        using_list(std::move(using_list)),
        const_declaration_list(std::move(const_declaration_list)),
        protocol_declaration_list(std::move(protocol_declaration_list)),
        resource_declaration_list(std::move(resource_declaration_list)),
        service_declaration_list(std::move(service_declaration_list)),
        type_decls(std::move(type_decls)),
        tokens(std::move(tokens)),
        end_(end) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<LibraryDeclaration> library_decl;
  std::vector<std::unique_ptr<AliasDeclaration>> alias_list;
  std::vector<std::unique_ptr<Using>> using_list;
  std::vector<std::unique_ptr<ConstDeclaration>> const_declaration_list;
  std::vector<std::unique_ptr<ProtocolDeclaration>> protocol_declaration_list;
  std::vector<std::unique_ptr<ResourceDeclaration>> resource_declaration_list;
  std::vector<std::unique_ptr<ServiceDeclaration>> service_declaration_list;
  std::vector<std::unique_ptr<TypeDeclaration>> type_decls;

  // An ordered list of all tokens (including comments) in the source file.
  std::vector<Token> tokens;
  Token end_;
};

}  // namespace fidl::raw

namespace std {

// The entire purpose of |SourceElement::Signature| is to be used as a map key. This is a simple
// hashing function over that object.
template <>
struct std::hash<fidl::raw::SourceElement::Signature> {
  std::size_t operator()(const fidl::raw::SourceElement::Signature& signature) const noexcept {
    using NodeKindType = typename std::underlying_type<fidl::raw::SourceElement::NodeKind>::type;
    std::size_t a = std::hash<NodeKindType>{}(static_cast<NodeKindType>(signature.node_kind_));
    std::size_t b = std::hash<const void*>{}(signature.data_.data());
    std::size_t c = std::hash<size_t>{}(signature.data_.size());
    return a ^ (b << 1) ^ (c << 2);
  }
};

}  // namespace std

#endif  // TOOLS_FIDL_FIDLC_INCLUDE_FIDL_RAW_AST_H_
