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

class TreeVisitor;

// A collection of one or more consecutive |Token|s from a single |SourceFile|.
// Each AST node has a SourceElement to keep track of where it came from.
struct SourceElement {
  SourceElement(Token start, Token end) : start_token(start), end_token(end) {}
  virtual ~SourceElement() = default;

  bool has_span() const {
    return start_token.span().valid() && end_token.span().valid() &&
           &start_token.span().source_file() == &end_token.span().source_file();
  }

  SourceSpan span() const {
    if (!start_token.span().valid() || !end_token.span().valid()) {
      return SourceSpan();
    }

    ZX_ASSERT(has_span());
    const char* start_pos = start_token.ptr();
    const char* end_pos = end_token.ptr() + end_token.data().length();
    return SourceSpan(std::string_view(start_pos, end_pos - start_pos),
                      start_token.span().source_file());
  }

  Token start_token;
  Token end_token;
};

// RAII helper class for calling OnSourceElementStart and OnSourceElementEnd.
class SourceElementMark {
 public:
  SourceElementMark(TreeVisitor* tv, const SourceElement& element);
  ~SourceElementMark();

 private:
  TreeVisitor* tv_;
  const SourceElement& element_;
};

struct Identifier final : public SourceElement {
  explicit Identifier(const SourceElement& element) : SourceElement(element) {}

  void Accept(TreeVisitor* visitor) const;
};

struct CompoundIdentifier final : public SourceElement {
  CompoundIdentifier(const SourceElement& element,
                     std::vector<std::unique_ptr<Identifier>> components)
      : SourceElement(element), components(std::move(components)) {}

  void Accept(TreeVisitor* visitor) const;

  std::vector<std::unique_ptr<Identifier>> components;
};

struct Literal : public SourceElement {
  enum class Kind : uint8_t {
    kBool,
    kDocComment,
    kNumeric,
    kString,
  };

  explicit Literal(const SourceElement& element, Kind kind) : SourceElement(element), kind(kind) {}

  const Kind kind;
};

struct DocCommentLiteral final : public Literal {
  explicit DocCommentLiteral(const SourceElement& element) : Literal(element, Kind::kDocComment) {}

  void Accept(TreeVisitor* visitor) const;

  std::string MakeContents() const {
    if (!has_span() || span().data().empty()) {
      return "";
    }
    return fidl::utils::strip_doc_comment_slashes(span().data());
  }
};

struct StringLiteral final : public Literal {
  explicit StringLiteral(const SourceElement& element) : Literal(element, Kind::kString) {}

  void Accept(TreeVisitor* visitor) const;

  std::string MakeContents() const {
    if (!has_span() || span().data().empty()) {
      return "";
    }
    return fidl::utils::strip_string_literal_quotes(span().data());
  }
};

struct NumericLiteral final : public Literal {
  explicit NumericLiteral(const SourceElement& element) : Literal(element, Kind::kNumeric) {}

  void Accept(TreeVisitor* visitor) const;
};

struct BoolLiteral final : public Literal {
  BoolLiteral(const SourceElement& element, bool value)
      : Literal(element, Kind::kBool), value(value) {}

  void Accept(TreeVisitor* visitor) const;

  const bool value;
};

struct Ordinal64 final : public SourceElement {
  Ordinal64(const SourceElement& element, uint64_t value) : SourceElement(element), value(value) {}

  void Accept(TreeVisitor* visitor) const;

  const uint64_t value;
};

struct Constant : public SourceElement {
  enum class Kind : uint8_t { kIdentifier, kLiteral, kBinaryOperator };

  explicit Constant(Token start, Token end, Kind kind) : SourceElement(start, end), kind(kind) {}
  explicit Constant(const SourceElement& element, Kind kind) : SourceElement(element), kind(kind) {}

  const Kind kind;
};

struct IdentifierConstant final : public Constant {
  explicit IdentifierConstant(std::unique_ptr<CompoundIdentifier> identifier)
      : Constant(SourceElement(identifier->start_token, identifier->end_token), Kind::kIdentifier),
        identifier(std::move(identifier)) {}

  std::unique_ptr<CompoundIdentifier> identifier;

  void Accept(TreeVisitor* visitor) const;
};

struct LiteralConstant final : public Constant {
  explicit LiteralConstant(std::unique_ptr<Literal> literal)
      : Constant(literal->start_token, literal->end_token, Kind::kLiteral),
        literal(std::move(literal)) {}

  std::unique_ptr<Literal> literal;

  void Accept(TreeVisitor* visitor) const;
};

struct BinaryOperatorConstant final : public Constant {
  enum class Operator : uint8_t { kOr };
  explicit BinaryOperatorConstant(std::unique_ptr<Constant> left_operand,
                                  std::unique_ptr<Constant> right_operand, Operator op)
      : Constant(SourceElement(left_operand->start_token, right_operand->end_token),
                 Kind::kBinaryOperator),
        left_operand(std::move(left_operand)),
        right_operand(std::move(right_operand)),
        op(op) {}

  std::unique_ptr<Constant> left_operand;
  std::unique_ptr<Constant> right_operand;
  Operator op;

  void Accept(TreeVisitor* visitor) const;
};

struct AttributeArg final : public SourceElement {
  // Constructor for cases where the arg name has been explicitly defined in the text.
  AttributeArg(const SourceElement& element, std::unique_ptr<Identifier> name,
               std::unique_ptr<Constant> value)
      : SourceElement(element), maybe_name(std::move(name)), value(std::move(value)) {}

  // Constructor for cases where the arg name is inferred.
  AttributeArg(const SourceElement& element, std::unique_ptr<Constant> value)
      : SourceElement(element), maybe_name(nullptr), value(std::move(value)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<Identifier> maybe_name;
  std::unique_ptr<Constant> value;
};

struct Attribute final : public SourceElement {
  enum class Provenance : uint8_t {
    kDefault,
    kDocComment,
  };

  // Constructor for cases where the name of the attribute is explicitly defined in the text.
  Attribute(const SourceElement& element, std::unique_ptr<Identifier> maybe_name,
            std::vector<std::unique_ptr<AttributeArg>> args)
      : SourceElement(element), maybe_name(std::move(maybe_name)), args(std::move(args)) {}

  // Factory for "///"-style doc comments, which have no attribute name.
  static Attribute CreateDocComment(const SourceElement& element,
                                    std::vector<std::unique_ptr<AttributeArg>> args) {
    auto attr = Attribute(element, nullptr, std::move(args));
    attr.provenance = Provenance::kDocComment;
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
struct AttributeList final : public SourceElement {
  AttributeList(const SourceElement& element, std::vector<std::unique_ptr<Attribute>> attributes)
      : SourceElement(element), attributes(std::move(attributes)) {}

  void Accept(TreeVisitor* visitor) const;

  std::vector<std::unique_ptr<Attribute>> attributes;
};

struct TypeConstructor;

struct LayoutReference;
struct LayoutParameterList;
struct TypeConstraints;

// The monostate variant is used to represent a parse failure.
using ConstraintOrSubtype = std::variant<std::unique_ptr<TypeConstraints>,
                                         std::unique_ptr<TypeConstructor>, std::monostate>;

struct TypeConstructor final : public SourceElement {
  TypeConstructor(const SourceElement& element, std::unique_ptr<LayoutReference> layout_ref,
                  std::unique_ptr<LayoutParameterList> parameters,
                  std::unique_ptr<TypeConstraints> constraints)
      : SourceElement(element),
        layout_ref(std::move(layout_ref)),
        parameters(std::move(parameters)),
        constraints(std::move(constraints)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<LayoutReference> layout_ref;
  std::unique_ptr<LayoutParameterList> parameters;
  std::unique_ptr<TypeConstraints> constraints;
};

struct AliasDeclaration final : public SourceElement {
  AliasDeclaration(const SourceElement& element, std::unique_ptr<AttributeList> attributes,
                   std::unique_ptr<Identifier> alias, std::unique_ptr<TypeConstructor> type_ctor)
      : SourceElement(element),
        attributes(std::move(attributes)),
        alias(std::move(alias)),
        type_ctor(std::move(type_ctor)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<AttributeList> attributes;
  std::unique_ptr<Identifier> alias;
  std::unique_ptr<TypeConstructor> type_ctor;
};

struct LibraryDeclaration final : public SourceElement {
  LibraryDeclaration(const SourceElement& element, std::unique_ptr<AttributeList> attributes,
                     std::unique_ptr<CompoundIdentifier> path)
      : SourceElement(element), attributes(std::move(attributes)), path(std::move(path)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<AttributeList> attributes;
  std::unique_ptr<CompoundIdentifier> path;
};

struct Using final : public SourceElement {
  Using(const SourceElement& element, std::unique_ptr<AttributeList> attributes,
        std::unique_ptr<CompoundIdentifier> using_path, std::unique_ptr<Identifier> maybe_alias)
      : SourceElement(element),
        attributes(std::move(attributes)),
        using_path(std::move(using_path)),
        maybe_alias(std::move(maybe_alias)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<AttributeList> attributes;
  std::unique_ptr<CompoundIdentifier> using_path;
  std::unique_ptr<Identifier> maybe_alias;
};

struct ConstDeclaration final : public SourceElement {
  ConstDeclaration(const SourceElement& element, std::unique_ptr<AttributeList> attributes,
                   std::unique_ptr<TypeConstructor> type_ctor,
                   std::unique_ptr<Identifier> identifier, std::unique_ptr<Constant> constant)
      : SourceElement(element),
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
struct Modifier final {
  Modifier(T value, Token token) : value(value), token(token) {}

  // Value of the modifier
  T value;
  // Token that the modifier is from.
  Token token;
};

struct Modifiers final : public SourceElement {
  // Constructor for Layouts (has resourceness and strictness, but not openness).
  Modifiers(const SourceElement& element,
            std::optional<Modifier<types::Resourceness>> maybe_resourceness,
            std::optional<Modifier<types::Strictness>> maybe_strictness,
            bool resourceness_comes_first)
      : SourceElement(element),
        maybe_resourceness(maybe_resourceness),
        maybe_strictness(maybe_strictness),
        maybe_openness(std::nullopt),
        resourceness_comes_first(resourceness_comes_first) {}

  // Constructor for Protocols (only has openness).
  Modifiers(const SourceElement& element, std::optional<Modifier<types::Openness>> maybe_openness)
      : SourceElement(element),
        maybe_resourceness(std::nullopt),
        maybe_strictness(std::nullopt),
        maybe_openness(maybe_openness),
        resourceness_comes_first(false) {}

  // Constructor for Protocol methods (only has strictness).
  Modifiers(const SourceElement& element,
            std::optional<Modifier<types::Strictness>> maybe_strictness)
      : SourceElement(element),
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

struct ParameterList final : public SourceElement {
  ParameterList(const SourceElement& element, std::unique_ptr<TypeConstructor> type_ctor)
      : SourceElement(element), type_ctor(std::move(type_ctor)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<TypeConstructor> type_ctor;
};

struct ProtocolMethod : public SourceElement {
  ProtocolMethod(const SourceElement& element, std::unique_ptr<AttributeList> attributes,
                 std::unique_ptr<Modifiers> modifiers, std::unique_ptr<Identifier> identifier,
                 std::unique_ptr<ParameterList> maybe_request,
                 std::unique_ptr<ParameterList> maybe_response,
                 std::unique_ptr<TypeConstructor> maybe_error_ctor)
      : SourceElement(element),
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

struct ProtocolCompose final : public SourceElement {
  ProtocolCompose(const SourceElement& element, std::unique_ptr<AttributeList> attributes,
                  std::unique_ptr<CompoundIdentifier> protocol_name)
      : SourceElement(element),
        attributes(std::move(attributes)),
        protocol_name(std::move(protocol_name)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<AttributeList> attributes;
  std::unique_ptr<CompoundIdentifier> protocol_name;
};

struct ProtocolDeclaration final : public SourceElement {
  ProtocolDeclaration(const SourceElement& element, std::unique_ptr<AttributeList> attributes,
                      std::unique_ptr<Modifiers> modifiers, std::unique_ptr<Identifier> identifier,
                      std::vector<std::unique_ptr<ProtocolCompose>> composed_protocols,
                      std::vector<std::unique_ptr<ProtocolMethod>> methods)
      : SourceElement(element),
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

struct ResourceProperty final : public SourceElement {
  ResourceProperty(const SourceElement& element, std::unique_ptr<TypeConstructor> type_ctor,
                   std::unique_ptr<Identifier> identifier,
                   std::unique_ptr<AttributeList> attributes)
      : SourceElement(element),
        type_ctor(std::move(type_ctor)),
        identifier(std::move(identifier)),
        attributes(std::move(attributes)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<TypeConstructor> type_ctor;
  std::unique_ptr<Identifier> identifier;
  std::unique_ptr<AttributeList> attributes;
};

struct ResourceDeclaration final : public SourceElement {
  ResourceDeclaration(const SourceElement& element, std::unique_ptr<AttributeList> attributes,
                      std::unique_ptr<Identifier> identifier,
                      std::unique_ptr<TypeConstructor> maybe_type_ctor,
                      std::vector<std::unique_ptr<ResourceProperty>> properties)
      : SourceElement(element),
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

struct ServiceMember final : public SourceElement {
  ServiceMember(const SourceElement& element, std::unique_ptr<TypeConstructor> type_ctor,
                std::unique_ptr<Identifier> identifier, std::unique_ptr<AttributeList> attributes)
      : SourceElement(element),
        type_ctor(std::move(type_ctor)),
        identifier(std::move(identifier)),
        attributes(std::move(attributes)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<TypeConstructor> type_ctor;
  std::unique_ptr<Identifier> identifier;
  std::unique_ptr<AttributeList> attributes;
};

struct ServiceDeclaration final : public SourceElement {
  ServiceDeclaration(const SourceElement& element, std::unique_ptr<AttributeList> attributes,
                     std::unique_ptr<Identifier> identifier,
                     std::vector<std::unique_ptr<ServiceMember>> members)
      : SourceElement(element),
        attributes(std::move(attributes)),
        identifier(std::move(identifier)),
        members(std::move(members)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<AttributeList> attributes;
  std::unique_ptr<Identifier> identifier;
  std::vector<std::unique_ptr<ServiceMember>> members;
};

struct LayoutMember : public SourceElement {
  enum class Kind : uint8_t {
    kOrdinaled,
    kStruct,
    kValue,
  };

  explicit LayoutMember(const SourceElement& element, Kind kind,
                        std::unique_ptr<AttributeList> attributes,
                        std::unique_ptr<Identifier> identifier)
      : SourceElement(element),
        kind(kind),
        attributes(std::move(attributes)),
        identifier(std::move(identifier)) {}

  void Accept(TreeVisitor* visitor) const;

  const Kind kind;
  std::unique_ptr<AttributeList> attributes;
  std::unique_ptr<Identifier> identifier;
};

struct Layout final : public SourceElement {
  enum class Kind : uint8_t {
    kBits,
    kEnum,
    kStruct,
    kTable,
    kUnion,
    kOverlay,
  };

  Layout(const SourceElement& element, Kind kind,
         std::vector<std::unique_ptr<LayoutMember>> members, std::unique_ptr<Modifiers> modifiers,
         std::unique_ptr<TypeConstructor> subtype_ctor)
      : SourceElement(element),
        kind(kind),
        members(std::move(members)),
        modifiers(std::move(modifiers)),
        subtype_ctor(std::move(subtype_ctor)) {}

  void Accept(TreeVisitor* visitor) const;

  Kind kind;
  std::vector<std::unique_ptr<raw::LayoutMember>> members;
  std::unique_ptr<Modifiers> modifiers;
  // Only used for Kind::kBits and Kind::kEnum.
  std::unique_ptr<TypeConstructor> subtype_ctor;
};

struct OrdinaledLayoutMember final : public LayoutMember {
  explicit OrdinaledLayoutMember(const SourceElement& element,
                                 std::unique_ptr<AttributeList> attributes,
                                 std::unique_ptr<Ordinal64> ordinal,
                                 std::unique_ptr<Identifier> identifier,
                                 std::unique_ptr<TypeConstructor> type_ctor)
      : LayoutMember(element, Kind::kOrdinaled, std::move(attributes), std::move(identifier)),
        ordinal(std::move(ordinal)),
        type_ctor(std::move(type_ctor)) {}
  explicit OrdinaledLayoutMember(const SourceElement& element,
                                 std::unique_ptr<AttributeList> attributes,
                                 std::unique_ptr<Ordinal64> ordinal)
      : LayoutMember(element, Kind::kOrdinaled, std::move(attributes), nullptr),
        ordinal(std::move(ordinal)),
        type_ctor(nullptr),
        reserved(true) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<Ordinal64> ordinal;
  std::unique_ptr<TypeConstructor> type_ctor;
  const bool reserved = false;
};

struct ValueLayoutMember final : public LayoutMember {
  explicit ValueLayoutMember(const SourceElement& element,
                             std::unique_ptr<AttributeList> attributes,
                             std::unique_ptr<Identifier> identifier,
                             std::unique_ptr<Constant> value)
      : LayoutMember(element, Kind::kValue, std::move(attributes), std::move(identifier)),
        value(std::move(value)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<Constant> value;
};

struct StructLayoutMember final : public LayoutMember {
  explicit StructLayoutMember(const SourceElement& element,
                              std::unique_ptr<AttributeList> attributes,
                              std::unique_ptr<Identifier> identifier,
                              std::unique_ptr<TypeConstructor> type_ctor,
                              std::unique_ptr<Constant> default_value)
      : LayoutMember(element, Kind::kStruct, std::move(attributes), std::move(identifier)),
        type_ctor(std::move(type_ctor)),
        default_value(std::move(default_value)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<TypeConstructor> type_ctor;
  std::unique_ptr<Constant> default_value;
};

struct LayoutReference : public SourceElement {
  enum class Kind : uint8_t {
    kInline,
    kNamed,
  };

  LayoutReference(const SourceElement& element, Kind kind) : SourceElement(element), kind(kind) {}

  void Accept(TreeVisitor* visitor) const;
  const Kind kind;
};

struct InlineLayoutReference final : public LayoutReference {
  explicit InlineLayoutReference(const SourceElement& element,
                                 std::unique_ptr<AttributeList> attributes,
                                 std::unique_ptr<Layout> layout)
      : LayoutReference(element, Kind::kInline),
        attributes(std::move(attributes)),
        layout(std::move(layout)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<AttributeList> attributes;
  std::unique_ptr<Layout> layout;
};

struct NamedLayoutReference final : public LayoutReference {
  explicit NamedLayoutReference(const SourceElement& element,
                                std::unique_ptr<CompoundIdentifier> identifier)
      : LayoutReference(element, Kind::kNamed), identifier(std::move(identifier)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<CompoundIdentifier> identifier;
};

struct LayoutParameter : public SourceElement {
  enum class Kind : uint8_t {
    kIdentifier,
    kLiteral,
    kType,
  };

  LayoutParameter(const SourceElement& element, Kind kind) : SourceElement(element), kind(kind) {}

  void Accept(TreeVisitor* visitor) const;

  const Kind kind;
};

struct LiteralLayoutParameter final : public LayoutParameter {
  explicit LiteralLayoutParameter(const SourceElement& element,
                                  std::unique_ptr<LiteralConstant> literal)
      : LayoutParameter(element, Kind::kLiteral), literal(std::move(literal)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<LiteralConstant> literal;
};

struct TypeLayoutParameter final : public LayoutParameter {
  explicit TypeLayoutParameter(const SourceElement& element,
                               std::unique_ptr<TypeConstructor> type_ctor)
      : LayoutParameter(element, Kind::kType), type_ctor(std::move(type_ctor)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<TypeConstructor> type_ctor;
};

struct IdentifierLayoutParameter final : public LayoutParameter {
  explicit IdentifierLayoutParameter(const SourceElement& element,
                                     std::unique_ptr<CompoundIdentifier> identifier)
      : LayoutParameter(element, Kind::kIdentifier), identifier(std::move(identifier)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<CompoundIdentifier> identifier;
};

struct LayoutParameterList final : public SourceElement {
  LayoutParameterList(const SourceElement& element,
                      std::vector<std::unique_ptr<raw::LayoutParameter>> items)
      : SourceElement(element), items(std::move(items)) {}

  void Accept(TreeVisitor* visitor) const;

  std::vector<std::unique_ptr<raw::LayoutParameter>> items;
};

struct TypeConstraints final : public SourceElement {
  TypeConstraints(const SourceElement& element, std::vector<std::unique_ptr<raw::Constant>> items)
      : SourceElement(element), items(std::move(items)) {}

  void Accept(TreeVisitor* visitor) const;

  std::vector<std::unique_ptr<raw::Constant>> items;
};

struct TypeDeclaration final : public SourceElement {
  TypeDeclaration(const SourceElement& element, std::unique_ptr<AttributeList> attributes,
                  std::unique_ptr<Identifier> identifier,
                  std::unique_ptr<TypeConstructor> type_ctor)
      : SourceElement(element),
        attributes(std::move(attributes)),
        identifier(std::move(identifier)),
        type_ctor(std::move(type_ctor)) {}

  void Accept(TreeVisitor* visitor) const;

  std::unique_ptr<AttributeList> attributes;
  std::unique_ptr<Identifier> identifier;
  std::unique_ptr<TypeConstructor> type_ctor;
};

struct File final : public SourceElement {
  File(const SourceElement& element, std::unique_ptr<LibraryDeclaration> library_decl,
       std::vector<std::unique_ptr<AliasDeclaration>> alias_list,
       std::vector<std::unique_ptr<Using>> using_list,
       std::vector<std::unique_ptr<ConstDeclaration>> const_declaration_list,
       std::vector<std::unique_ptr<ProtocolDeclaration>> protocol_declaration_list,
       std::vector<std::unique_ptr<ResourceDeclaration>> resource_declaration_list,
       std::vector<std::unique_ptr<ServiceDeclaration>> service_declaration_list,
       std::vector<std::unique_ptr<TypeDeclaration>> type_decls, std::vector<Token> tokens)
      : SourceElement(element),
        library_decl(std::move(library_decl)),
        alias_list(std::move(alias_list)),
        using_list(std::move(using_list)),
        const_declaration_list(std::move(const_declaration_list)),
        protocol_declaration_list(std::move(protocol_declaration_list)),
        resource_declaration_list(std::move(resource_declaration_list)),
        service_declaration_list(std::move(service_declaration_list)),
        type_decls(std::move(type_decls)),
        tokens(std::move(tokens)) {}

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
};

}  // namespace fidl::raw

#endif  // TOOLS_FIDL_FIDLC_INCLUDE_FIDL_RAW_AST_H_
