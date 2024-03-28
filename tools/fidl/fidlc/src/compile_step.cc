// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "tools/fidl/fidlc/src/compile_step.h"

#include <zircon/assert.h>

#include <algorithm>
#include <optional>

#include "tools/fidl/fidlc/src/attribute_schema.h"
#include "tools/fidl/fidlc/src/diagnostics.h"
#include "tools/fidl/fidlc/src/experimental_flags.h"
#include "tools/fidl/fidlc/src/flat_ast.h"
#include "tools/fidl/fidlc/src/name.h"
#include "tools/fidl/fidlc/src/names.h"
#include "tools/fidl/fidlc/src/ordinals.h"
#include "tools/fidl/fidlc/src/type_resolver.h"

namespace fidlc {

// See RFC-0132 for the origin of this table limit.
constexpr size_t kMaxTableOrdinals = 64;

void CompileStep::RunImpl() {
  CompileAttributeList(library()->attributes.get());
  for (auto& [name, decl] : library()->declarations.all) {
    CompileDecl(decl);
  }
}

namespace {

class ScopeInsertResult {
 public:
  explicit ScopeInsertResult(std::unique_ptr<SourceSpan> previous_occurrence)
      : previous_occurrence_(std::move(previous_occurrence)) {}

  static ScopeInsertResult Ok() { return ScopeInsertResult(nullptr); }
  static ScopeInsertResult FailureAt(SourceSpan previous) {
    return ScopeInsertResult(std::make_unique<SourceSpan>(previous));
  }

  bool ok() const { return previous_occurrence_ == nullptr; }

  const SourceSpan& previous_occurrence() const {
    ZX_ASSERT(!ok());
    return *previous_occurrence_;
  }

 private:
  std::unique_ptr<SourceSpan> previous_occurrence_;
};

template <typename T>
class Scope {
 public:
  ScopeInsertResult Insert(const T& t, SourceSpan span) {
    auto iter = scope_.find(t);
    if (iter != scope_.end()) {
      return ScopeInsertResult::FailureAt(iter->second);
    }
    scope_.emplace(t, span);
    return ScopeInsertResult::Ok();
  }

  typename std::map<T, SourceSpan>::const_iterator begin() const { return scope_.begin(); }

  typename std::map<T, SourceSpan>::const_iterator end() const { return scope_.end(); }

 private:
  std::map<T, SourceSpan> scope_;
};

using Ordinal64Scope = Scope<uint64_t>;

struct MethodScope {
  Ordinal64Scope ordinals;
  Scope<std::string> canonical_names;
  Scope<const Protocol*> protocols;
};

}  // namespace

void CompileStep::CompileDecl(Decl* decl) {
  if (decl->name.library() != library()) {
    ZX_ASSERT_MSG(decl->state == Decl::State::kCompiled,
                  "decls in dependencies must already be compiled");
  }
  switch (decl->state) {
    case Decl::State::kNotCompiled:
      break;
    case Decl::State::kCompiled:
      return;
    case Decl::State::kCompiling: {
      auto it = std::find(decl_stack_.begin(), decl_stack_.end(), decl);
      ZX_ASSERT_MSG(it != decl_stack_.end(), "kCompiling decl should be in decl_stack_");
      std::vector<const Decl*> cycle(it, decl_stack_.end());
      cycle.push_back(decl);
      reporter()->Fail(ErrIncludeCycle, decl->name.span().value(), cycle);
      return;
    }
  }
  decl->state = Decl::State::kCompiling;
  decl_stack_.push_back(decl);
  switch (decl->kind) {
    case Decl::Kind::kBuiltin:
      // Nothing to do.
      break;
    case Decl::Kind::kBits:
      CompileBits(static_cast<Bits*>(decl));
      break;
    case Decl::Kind::kConst:
      CompileConst(static_cast<Const*>(decl));
      break;
    case Decl::Kind::kEnum:
      CompileEnum(static_cast<Enum*>(decl));
      break;
    case Decl::Kind::kProtocol:
      CompileProtocol(static_cast<Protocol*>(decl));
      break;
    case Decl::Kind::kResource:
      CompileResource(static_cast<Resource*>(decl));
      break;
    case Decl::Kind::kService:
      CompileService(static_cast<Service*>(decl));
      break;
    case Decl::Kind::kStruct:
      CompileStruct(static_cast<Struct*>(decl));
      break;
    case Decl::Kind::kTable:
      CompileTable(static_cast<Table*>(decl));
      break;
    case Decl::Kind::kUnion:
      CompileUnion(static_cast<Union*>(decl));
      break;
    case Decl::Kind::kOverlay:
      CompileOverlay(static_cast<Overlay*>(decl));
      break;
    case Decl::Kind::kAlias:
      CompileAlias(static_cast<Alias*>(decl));
      break;
    case Decl::Kind::kNewType:
      CompileNewType(static_cast<NewType*>(decl));
      break;
  }  // switch
  decl->state = Decl::State::kCompiled;
  decl_stack_.pop_back();
  library()->declaration_order.push_back(decl);
}

bool CompileStep::ResolveOrOperatorConstant(Constant* constant, std::optional<const Type*> opt_type,
                                            const ConstantValue& left_operand,
                                            const ConstantValue& right_operand) {
  ZX_ASSERT_MSG(left_operand.kind == right_operand.kind,
                "left and right operands of or operator must be of the same kind");
  ZX_ASSERT_MSG(opt_type, "type inference not implemented for or operator");
  const auto type = UnderlyingType(opt_type.value());
  if (type == nullptr)
    return false;
  if (type->kind != Type::Kind::kPrimitive) {
    return reporter()->Fail(ErrOrOperatorOnNonPrimitiveValue, constant->span);
  }
  std::unique_ptr<ConstantValue> left_operand_u64;
  std::unique_ptr<ConstantValue> right_operand_u64;
  if (!left_operand.Convert(ConstantValue::Kind::kUint64, &left_operand_u64))
    return false;
  if (!right_operand.Convert(ConstantValue::Kind::kUint64, &right_operand_u64))
    return false;
  NumericConstantValue<uint64_t> result =
      *static_cast<NumericConstantValue<uint64_t>*>(left_operand_u64.get()) |
      *static_cast<NumericConstantValue<uint64_t>*>(right_operand_u64.get());
  std::unique_ptr<ConstantValue> converted_result;
  if (!result.Convert(ConstantValuePrimitiveKind(static_cast<const PrimitiveType*>(type)->subtype),
                      &converted_result))
    return false;
  constant->ResolveTo(std::move(converted_result), type);
  return true;
}

bool CompileStep::ResolveConstant(Constant* constant, std::optional<const Type*> opt_type) {
  ZX_ASSERT(constant != nullptr);

  // Prevent re-entry.
  if (constant->compiled)
    return constant->IsResolved();
  constant->compiled = true;

  switch (constant->kind) {
    case Constant::Kind::kIdentifier:
      return ResolveIdentifierConstant(static_cast<IdentifierConstant*>(constant), opt_type);
    case Constant::Kind::kLiteral:
      return ResolveLiteralConstant(static_cast<LiteralConstant*>(constant), opt_type);
    case Constant::Kind::kBinaryOperator: {
      auto binary_operator_constant = static_cast<BinaryOperatorConstant*>(constant);
      if (!ResolveConstant(binary_operator_constant->left_operand.get(), opt_type)) {
        return false;
      }
      if (!ResolveConstant(binary_operator_constant->right_operand.get(), opt_type)) {
        return false;
      }
      switch (binary_operator_constant->op) {
        case BinaryOperatorConstant::Operator::kOr:
          return ResolveOrOperatorConstant(constant, opt_type,
                                           binary_operator_constant->left_operand->Value(),
                                           binary_operator_constant->right_operand->Value());
        default:
          ZX_PANIC("unhandled binary operator");
      }
    }
  }
}

ConstantValue::Kind CompileStep::ConstantValuePrimitiveKind(
    const PrimitiveSubtype primitive_subtype) {
  switch (primitive_subtype) {
    case PrimitiveSubtype::kBool:
      return ConstantValue::Kind::kBool;
    case PrimitiveSubtype::kInt8:
      return ConstantValue::Kind::kInt8;
    case PrimitiveSubtype::kInt16:
      return ConstantValue::Kind::kInt16;
    case PrimitiveSubtype::kInt32:
      return ConstantValue::Kind::kInt32;
    case PrimitiveSubtype::kInt64:
      return ConstantValue::Kind::kInt64;
    case PrimitiveSubtype::kUint8:
      return ConstantValue::Kind::kUint8;
    case PrimitiveSubtype::kZxUchar:
      return ConstantValue::Kind::kZxUchar;
    case PrimitiveSubtype::kUint16:
      return ConstantValue::Kind::kUint16;
    case PrimitiveSubtype::kUint32:
      return ConstantValue::Kind::kUint32;
    case PrimitiveSubtype::kUint64:
      return ConstantValue::Kind::kUint64;
    case PrimitiveSubtype::kZxUsize64:
      return ConstantValue::Kind::kZxUsize64;
    case PrimitiveSubtype::kZxUintptr64:
      return ConstantValue::Kind::kZxUintptr64;
    case PrimitiveSubtype::kFloat32:
      return ConstantValue::Kind::kFloat32;
    case PrimitiveSubtype::kFloat64:
      return ConstantValue::Kind::kFloat64;
  }
}

bool CompileStep::ResolveIdentifierConstant(IdentifierConstant* identifier_constant,
                                            std::optional<const Type*> opt_type) {
  if (opt_type) {
    ZX_ASSERT_MSG(TypeCanBeConst(opt_type.value()),
                  "resolving identifier constant to non-const-able type");
  }

  auto& reference = identifier_constant->reference;
  Decl* parent = reference.resolved().element_or_parent_decl();
  Element* target = reference.resolved().element();
  CompileDecl(parent);

  const Type* const_type = nullptr;
  const ConstantValue* const_val = nullptr;
  switch (target->kind) {
    case Element::Kind::kBuiltin: {
      // TODO(https://fxbug.dev/42182133): In some cases we want to return a more specific
      // error message from here, but right now we can't due to the way
      // TypeResolver::ResolveConstraintAs tries multiple interpretations.
      return false;
    }
    case Element::Kind::kConst: {
      auto const_decl = static_cast<Const*>(target);
      if (!const_decl->value->IsResolved()) {
        return false;
      }
      const_type = const_decl->type_ctor->type;
      const_val = &const_decl->value->Value();
      break;
    }
    case Element::Kind::kEnumMember: {
      ZX_ASSERT(parent->kind == Decl::Kind::kEnum);
      const_type = static_cast<Enum*>(parent)->subtype_ctor->type;
      auto member = static_cast<Enum::Member*>(target);
      if (!member->value->IsResolved()) {
        return false;
      }
      const_val = &member->value->Value();
      break;
    }
    case Element::Kind::kBitsMember: {
      ZX_ASSERT(parent->kind == Decl::Kind::kBits);
      const_type = static_cast<Bits*>(parent)->subtype_ctor->type;
      auto member = static_cast<Bits::Member*>(target);
      if (!member->value->IsResolved()) {
        return false;
      }
      const_val = &member->value->Value();
      break;
    }
    default: {
      return reporter()->Fail(ErrExpectedValueButGotType, reference.span(),
                              reference.resolved().name());
      break;
    }
  }

  ZX_ASSERT_MSG(const_val, "did not set const_val");
  ZX_ASSERT_MSG(const_type, "did not set const_type");

  std::unique_ptr<ConstantValue> resolved_val;
  const auto type = opt_type ? opt_type.value() : const_type;
  switch (type->kind) {
    case Type::Kind::kString: {
      if (!TypeIsConvertibleTo(const_type, type))
        goto fail_cannot_convert;

      if (!const_val->Convert(ConstantValue::Kind::kString, &resolved_val))
        goto fail_cannot_convert;
      break;
    }
    case Type::Kind::kPrimitive: {
      auto primitive_type = static_cast<const PrimitiveType*>(type);
      if (!const_val->Convert(ConstantValuePrimitiveKind(primitive_type->subtype), &resolved_val))
        goto fail_cannot_convert;
      break;
    }
    case Type::Kind::kIdentifier: {
      auto identifier_type = static_cast<const IdentifierType*>(type);
      CompileDecl(identifier_type->type_decl);
      const PrimitiveType* primitive_type;
      switch (identifier_type->type_decl->kind) {
        case Decl::Kind::kEnum: {
          auto enum_decl = static_cast<const Enum*>(identifier_type->type_decl);
          if (!enum_decl->subtype_ctor->type) {
            return false;
          }
          ZX_ASSERT(enum_decl->subtype_ctor->type->kind == Type::Kind::kPrimitive);
          primitive_type = static_cast<const PrimitiveType*>(enum_decl->subtype_ctor->type);
          break;
        }
        case Decl::Kind::kBits: {
          auto bits_decl = static_cast<const Bits*>(identifier_type->type_decl);
          ZX_ASSERT(bits_decl->subtype_ctor->type->kind == Type::Kind::kPrimitive);
          if (!bits_decl->subtype_ctor->type) {
            return false;
          }
          primitive_type = static_cast<const PrimitiveType*>(bits_decl->subtype_ctor->type);
          break;
        }
        default: {
          ZX_PANIC("identifier not of const-able type.");
        }
      }

      auto fail_with_mismatched_type = [this, identifier_type,
                                        identifier_constant](const Name& type_name) {
        return reporter()->Fail(ErrMismatchedNameTypeAssignment, identifier_constant->span,
                                identifier_type->type_decl->name, type_name);
      };

      switch (parent->kind) {
        case Decl::Kind::kConst: {
          if (const_type->name != identifier_type->type_decl->name)
            return fail_with_mismatched_type(const_type->name);
          break;
        }
        case Decl::Kind::kBits:
        case Decl::Kind::kEnum: {
          if (parent->name != identifier_type->type_decl->name)
            return fail_with_mismatched_type(parent->name);
          break;
        }
        default: {
          ZX_PANIC("identifier not of const-able type.");
        }
      }

      if (!const_val->Convert(ConstantValuePrimitiveKind(primitive_type->subtype), &resolved_val))
        goto fail_cannot_convert;
      break;
    }
    default: {
      ZX_PANIC("identifier not of const-able type.");
    }
  }

  identifier_constant->ResolveTo(std::move(resolved_val), type);
  return true;

fail_cannot_convert:
  return reporter()->Fail(ErrTypeCannotBeConvertedToType, reference.span(), identifier_constant,
                          const_type, type);
}

bool CompileStep::ResolveLiteralConstant(LiteralConstant* literal_constant,
                                         std::optional<const Type*> opt_type) {
  auto inferred_type = InferType(static_cast<Constant*>(literal_constant));
  const Type* type = opt_type ? opt_type.value() : inferred_type;
  if (!TypeIsConvertibleTo(inferred_type, type)) {
    return reporter()->Fail(ErrTypeCannotBeConvertedToType, literal_constant->literal->span(),
                            literal_constant, inferred_type, type);
  }
  switch (literal_constant->literal->kind) {
    case RawLiteral::Kind::kDocComment: {
      auto doc_comment_literal =
          static_cast<const RawDocCommentLiteral*>(literal_constant->literal);
      literal_constant->ResolveTo(
          std::make_unique<DocCommentConstantValue>(doc_comment_literal->span().data()),
          typespace()->GetUnboundedStringType());
      return true;
    }
    case RawLiteral::Kind::kString: {
      literal_constant->ResolveTo(
          std::make_unique<StringConstantValue>(literal_constant->literal->span().data()),
          typespace()->GetUnboundedStringType());
      return true;
    }
    case RawLiteral::Kind::kBool: {
      auto bool_literal = static_cast<const RawBoolLiteral*>(literal_constant->literal);
      literal_constant->ResolveTo(std::make_unique<BoolConstantValue>(bool_literal->value),
                                  typespace()->GetPrimitiveType(PrimitiveSubtype::kBool));
      return true;
    }
    case RawLiteral::Kind::kNumeric: {
      switch (type->kind) {
        case Type::Kind::kPrimitive:
          return ResolveLiteralConstantNumeric(literal_constant,
                                               static_cast<const PrimitiveType*>(type));
        case Type::Kind::kIdentifier: {
          ZX_ASSERT(static_cast<const IdentifierType*>(type)->type_decl->kind == Decl::Kind::kBits);
          auto underlying_type = UnderlyingType(type);
          if (underlying_type->kind != Type::Kind::kPrimitive)
            return false;
          auto primitive_type = static_cast<const PrimitiveType*>(underlying_type);
          if (!ResolveLiteralConstantNumeric(literal_constant, primitive_type))
            return false;
          auto& value = literal_constant->Value();
          uint64_t raw_value;
          switch (value.kind) {
            case ConstantValue::Kind::kUint8:
              raw_value = static_cast<const NumericConstantValue<uint8_t>&>(value).value;
              break;
            case ConstantValue::Kind::kUint16:
              raw_value = static_cast<const NumericConstantValue<uint16_t>&>(value).value;
              break;
            case ConstantValue::Kind::kUint32:
              raw_value = static_cast<const NumericConstantValue<uint32_t>&>(value).value;
              break;
            case ConstantValue::Kind::kUint64:
              raw_value = static_cast<const NumericConstantValue<uint64_t>&>(value).value;
              break;
            default:
              return false;
          }
          if (raw_value != 0) {
            return reporter()->Fail(ErrTypeCannotBeConvertedToType,
                                    literal_constant->literal->span(), literal_constant,
                                    inferred_type, type);
          }
          return true;
        }
        default:
          ZX_PANIC("TypeIsConvertibleTo should have returned false");
      }
    }
  }  // switch
}

bool CompileStep::ResolveLiteralConstantNumeric(LiteralConstant* literal_constant,
                                                const PrimitiveType* primitive_type) {
  switch (primitive_type->subtype) {
    case PrimitiveSubtype::kInt8:
      return ResolveLiteralConstantNumericImpl<int8_t>(literal_constant, primitive_type);
    case PrimitiveSubtype::kInt16:
      return ResolveLiteralConstantNumericImpl<int16_t>(literal_constant, primitive_type);
    case PrimitiveSubtype::kInt32:
      return ResolveLiteralConstantNumericImpl<int32_t>(literal_constant, primitive_type);
    case PrimitiveSubtype::kInt64:
      return ResolveLiteralConstantNumericImpl<int64_t>(literal_constant, primitive_type);
    case PrimitiveSubtype::kUint8:
    case PrimitiveSubtype::kZxUchar:
      return ResolveLiteralConstantNumericImpl<uint8_t>(literal_constant, primitive_type);
    case PrimitiveSubtype::kUint16:
      return ResolveLiteralConstantNumericImpl<uint16_t>(literal_constant, primitive_type);
    case PrimitiveSubtype::kUint32:
      return ResolveLiteralConstantNumericImpl<uint32_t>(literal_constant, primitive_type);
    case PrimitiveSubtype::kUint64:
    case PrimitiveSubtype::kZxUsize64:
    case PrimitiveSubtype::kZxUintptr64:
      return ResolveLiteralConstantNumericImpl<uint64_t>(literal_constant, primitive_type);
    case PrimitiveSubtype::kFloat32:
      return ResolveLiteralConstantNumericImpl<float>(literal_constant, primitive_type);
    case PrimitiveSubtype::kFloat64:
      return ResolveLiteralConstantNumericImpl<double>(literal_constant, primitive_type);
    default:
      ZX_PANIC("should not have any other primitive type reachable");
  }
}
template <typename NumericType>
bool CompileStep::ResolveLiteralConstantNumericImpl(LiteralConstant* literal_constant,
                                                    const PrimitiveType* primitive_type) {
  NumericType value;
  const auto span = literal_constant->literal->span();
  std::string string_data(span.data().data(), span.data().data() + span.data().size());
  switch (ParseNumeric(string_data, &value)) {
    case ParseNumericResult::kSuccess:
      literal_constant->ResolveTo(std::make_unique<NumericConstantValue<NumericType>>(value),
                                  primitive_type);
      return true;
    case ParseNumericResult::kMalformed:
      // The caller (ResolveLiteralConstant) ensures that the constant kind is
      // a numeric literal, which means that it follows the grammar for
      // numerical types. As a result, an error to parse the data here is due
      // to the data being too large, rather than bad input.
      [[fallthrough]];
    case ParseNumericResult::kOutOfBounds:
      return reporter()->Fail(ErrConstantOverflowsType, span, literal_constant, primitive_type);
  }
}

const Type* CompileStep::InferType(Constant* constant) {
  switch (constant->kind) {
    case Constant::Kind::kLiteral: {
      auto literal =
          static_cast<const RawLiteral*>(static_cast<const LiteralConstant*>(constant)->literal);
      switch (literal->kind) {
        case RawLiteral::Kind::kString: {
          auto string_literal = static_cast<const RawStringLiteral*>(literal);
          auto inferred_size = string_literal_length(string_literal->span().data());
          return typespace()->GetStringType(inferred_size);
        }
        case RawLiteral::Kind::kNumeric:
          return typespace()->GetUntypedNumericType();
        case RawLiteral::Kind::kBool:
          return typespace()->GetPrimitiveType(PrimitiveSubtype::kBool);
        case RawLiteral::Kind::kDocComment:
          return typespace()->GetUnboundedStringType();
      }
      return nullptr;
    }
    case Constant::Kind::kIdentifier:
      if (!ResolveConstant(constant, std::nullopt)) {
        return nullptr;
      }
      return constant->type;
    case Constant::Kind::kBinaryOperator:
      ZX_PANIC("type inference not implemented for binops");
  }
}

bool CompileStep::ResolveAsOptional(Constant* constant) {
  ZX_ASSERT(constant);

  if (constant->kind != Constant::Kind::kIdentifier)
    return false;

  auto identifier_constant = static_cast<IdentifierConstant*>(constant);
  auto element = identifier_constant->reference.resolved().element();
  if (element->kind != Element::Kind::kBuiltin)
    return false;
  auto builtin = static_cast<Builtin*>(element);
  return builtin->id == Builtin::Identity::kOptional;
}

void CompileStep::CompileAttributeList(AttributeList* attributes) {
  Scope<std::string> scope;
  for (auto& attribute : attributes->attributes) {
    const auto original_name = attribute->name.data();
    const auto canonical_name = canonicalize(original_name);
    const auto result = scope.Insert(canonical_name, attribute->name);
    if (!result.ok()) {
      const auto previous_span = result.previous_occurrence();
      if (original_name == previous_span.data()) {
        reporter()->Fail(ErrDuplicateAttribute, attribute->name, original_name, previous_span);
      } else {
        reporter()->Fail(ErrDuplicateAttributeCanonical, attribute->name, original_name,
                         previous_span.data(), previous_span, canonical_name);
      }
    }
    CompileAttribute(attribute.get());
  }
}

void CompileStep::CompileAttribute(Attribute* attribute, bool early) {
  if (attribute->compiled) {
    return;
  }

  Scope<std::string> scope;
  for (auto& arg : attribute->args) {
    if (!arg->name.has_value()) {
      continue;
    }
    const auto original_name = arg->name.value().data();
    const auto canonical_name = canonicalize(original_name);
    const auto result = scope.Insert(canonical_name, arg->name.value());
    if (!result.ok()) {
      const auto previous_span = result.previous_occurrence();
      if (original_name == previous_span.data()) {
        reporter()->Fail(ErrDuplicateAttributeArg, attribute->span, attribute, original_name,
                         previous_span);
      } else {
        reporter()->Fail(ErrDuplicateAttributeArgCanonical, attribute->span, attribute,
                         original_name, previous_span.data(), previous_span, canonical_name);
      }
    }
  }

  const AttributeSchema& schema = all_libraries()->RetrieveAttributeSchema(attribute);
  if (early) {
    ZX_ASSERT_MSG(schema.CanCompileEarly(), "attribute is not allowed to be compiled early");
  }
  schema.ResolveArgs(this, attribute);
  attribute->compiled = true;
}

// static
void CompileStep::CompileAttributeEarly(Compiler* compiler, Attribute* attribute) {
  CompileStep(compiler).CompileAttribute(attribute, /* early = */ true);
}

const Type* CompileStep::UnderlyingType(const Type* type) {
  if (type->kind != Type::Kind::kIdentifier) {
    return type;
  }
  auto identifier_type = static_cast<const IdentifierType*>(type);
  Decl* decl = identifier_type->type_decl;
  CompileDecl(decl);
  switch (decl->kind) {
    case Decl::Kind::kBits:
      return static_cast<const Bits*>(decl)->subtype_ctor->type;
    case Decl::Kind::kEnum:
      return static_cast<const Enum*>(decl)->subtype_ctor->type;
    default:
      return type;
  }
}

bool CompileStep::TypeCanBeConst(const Type* type) {
  switch (type->kind) {
    case Type::Kind::kString:
      return !type->IsNullable();
    case Type::Kind::kPrimitive:
      return true;
    case Type::Kind::kIdentifier: {
      auto identifier_type = static_cast<const IdentifierType*>(type);
      switch (identifier_type->type_decl->kind) {
        case Decl::Kind::kEnum:
        case Decl::Kind::kBits:
          return true;
        default:
          return false;
      }
    }
    default:
      return false;
  }  // switch
}

bool CompileStep::TypeIsConvertibleTo(const Type* from_type, const Type* to_type) {
  switch (to_type->kind) {
    case Type::Kind::kString: {
      if (from_type->kind != Type::Kind::kString)
        return false;

      auto from_string_type = static_cast<const StringType*>(from_type);
      auto to_string_type = static_cast<const StringType*>(to_type);

      if (!to_string_type->IsNullable() && from_string_type->IsNullable())
        return false;

      if (to_string_type->MaxSize() < from_string_type->MaxSize())
        return false;

      return true;
    }
    case Type::Kind::kPrimitive: {
      auto to_primitive_type = static_cast<const PrimitiveType*>(to_type);
      switch (from_type->kind) {
        case Type::Kind::kUntypedNumeric:
          return to_primitive_type->subtype != PrimitiveSubtype::kBool;
        case Type::Kind::kPrimitive:
          break;  // handled below
        default:
          return false;
      }
      auto from_primitive_type = static_cast<const PrimitiveType*>(from_type);
      switch (to_primitive_type->subtype) {
        case PrimitiveSubtype::kBool:
          return from_primitive_type->subtype == PrimitiveSubtype::kBool;
        default:
          // TODO(https://fxbug.dev/42069446): be more precise about convertibility, e.g. it should
          // not be allowed to convert a float to an int.
          return from_primitive_type->subtype != PrimitiveSubtype::kBool;
      }
    }
    case Type::Kind::kIdentifier: {
      // Allow kUntypedNumeric for `const NONE BitsType = 0;`.
      auto identifier_type = static_cast<const IdentifierType*>(to_type);
      return identifier_type->type_decl->kind == Decl::Kind::kBits &&
             from_type->kind == Type::Kind::kUntypedNumeric;
    }
    default:
      return false;
  }  // switch
}

void CompileStep::CompileBits(Bits* bits_declaration) {
  CompileAttributeList(bits_declaration->attributes.get());
  for (auto& member : bits_declaration->members) {
    CompileAttributeList(member.attributes.get());
  }

  CompileTypeConstructor(bits_declaration->subtype_ctor.get());
  if (!bits_declaration->subtype_ctor->type) {
    return;
  }

  if (bits_declaration->subtype_ctor->type->kind != Type::Kind::kPrimitive) {
    reporter()->Fail(ErrBitsTypeMustBeUnsignedIntegralPrimitive,
                     bits_declaration->name.span().value(), bits_declaration->subtype_ctor->type);
    return;
  }

  // Validate constants.
  auto primitive_type = static_cast<const PrimitiveType*>(bits_declaration->subtype_ctor->type);
  switch (primitive_type->subtype) {
    case PrimitiveSubtype::kUint8: {
      uint8_t mask;
      if (!ValidateBitsMembersAndCalcMask<uint8_t>(bits_declaration, &mask))
        return;
      bits_declaration->mask = mask;
      break;
    }
    case PrimitiveSubtype::kUint16: {
      uint16_t mask;
      if (!ValidateBitsMembersAndCalcMask<uint16_t>(bits_declaration, &mask))
        return;
      bits_declaration->mask = mask;
      break;
    }
    case PrimitiveSubtype::kUint32: {
      uint32_t mask;
      if (!ValidateBitsMembersAndCalcMask<uint32_t>(bits_declaration, &mask))
        return;
      bits_declaration->mask = mask;
      break;
    }
    case PrimitiveSubtype::kUint64: {
      uint64_t mask;
      if (!ValidateBitsMembersAndCalcMask<uint64_t>(bits_declaration, &mask))
        return;
      bits_declaration->mask = mask;
      break;
    }
    case PrimitiveSubtype::kBool:
    case PrimitiveSubtype::kInt8:
    case PrimitiveSubtype::kInt16:
    case PrimitiveSubtype::kInt32:
    case PrimitiveSubtype::kInt64:
    case PrimitiveSubtype::kZxUchar:
    case PrimitiveSubtype::kZxUsize64:
    case PrimitiveSubtype::kZxUintptr64:
    case PrimitiveSubtype::kFloat32:
    case PrimitiveSubtype::kFloat64:
      reporter()->Fail(ErrBitsTypeMustBeUnsignedIntegralPrimitive,
                       bits_declaration->name.span().value(), bits_declaration->subtype_ctor->type);
      return;
  }
}

void CompileStep::CompileConst(Const* const_declaration) {
  CompileAttributeList(const_declaration->attributes.get());
  CompileTypeConstructor(const_declaration->type_ctor.get());
  const auto* const_type = const_declaration->type_ctor->type;
  if (!const_type) {
    return;
  }
  if (!TypeCanBeConst(const_type)) {
    reporter()->Fail(ErrInvalidConstantType, const_declaration->name.span().value(), const_type);
  } else if (!ResolveConstant(const_declaration->value.get(), const_type)) {
    reporter()->Fail(ErrCannotResolveConstantValue, const_declaration->name.span().value());
  }
}

void CompileStep::CompileEnum(Enum* enum_declaration) {
  CompileAttributeList(enum_declaration->attributes.get());
  for (auto& member : enum_declaration->members) {
    CompileAttributeList(member.attributes.get());
  }

  CompileTypeConstructor(enum_declaration->subtype_ctor.get());
  if (!enum_declaration->subtype_ctor->type) {
    return;
  }

  if (enum_declaration->subtype_ctor->type->kind != Type::Kind::kPrimitive) {
    reporter()->Fail(ErrEnumTypeMustBeIntegralPrimitive, enum_declaration->name.span().value(),
                     enum_declaration->subtype_ctor->type);
    return;
  }

  // Validate constants.
  auto primitive_type = static_cast<const PrimitiveType*>(enum_declaration->subtype_ctor->type);
  enum_declaration->type = primitive_type;
  switch (primitive_type->subtype) {
    case PrimitiveSubtype::kInt8: {
      int8_t unknown_value;
      if (ValidateEnumMembersAndCalcUnknownValue<int8_t>(enum_declaration, &unknown_value)) {
        enum_declaration->unknown_value_signed = unknown_value;
      }
      break;
    }
    case PrimitiveSubtype::kInt16: {
      int16_t unknown_value;
      if (ValidateEnumMembersAndCalcUnknownValue<int16_t>(enum_declaration, &unknown_value)) {
        enum_declaration->unknown_value_signed = unknown_value;
      }
      break;
    }
    case PrimitiveSubtype::kInt32: {
      int32_t unknown_value;
      if (ValidateEnumMembersAndCalcUnknownValue<int32_t>(enum_declaration, &unknown_value)) {
        enum_declaration->unknown_value_signed = unknown_value;
      }
      break;
    }
    case PrimitiveSubtype::kInt64: {
      int64_t unknown_value;
      if (ValidateEnumMembersAndCalcUnknownValue<int64_t>(enum_declaration, &unknown_value)) {
        enum_declaration->unknown_value_signed = unknown_value;
      }
      break;
    }
    case PrimitiveSubtype::kUint8: {
      uint8_t unknown_value;
      if (ValidateEnumMembersAndCalcUnknownValue<uint8_t>(enum_declaration, &unknown_value)) {
        enum_declaration->unknown_value_unsigned = unknown_value;
      }
      break;
    }
    case PrimitiveSubtype::kUint16: {
      uint16_t unknown_value;
      if (ValidateEnumMembersAndCalcUnknownValue<uint16_t>(enum_declaration, &unknown_value)) {
        enum_declaration->unknown_value_unsigned = unknown_value;
      }
      break;
    }
    case PrimitiveSubtype::kUint32: {
      uint32_t unknown_value;
      if (ValidateEnumMembersAndCalcUnknownValue<uint32_t>(enum_declaration, &unknown_value)) {
        enum_declaration->unknown_value_unsigned = unknown_value;
      }
      break;
    }
    case PrimitiveSubtype::kUint64: {
      uint64_t unknown_value;
      if (ValidateEnumMembersAndCalcUnknownValue<uint64_t>(enum_declaration, &unknown_value)) {
        enum_declaration->unknown_value_unsigned = unknown_value;
      }
      break;
    }
    case PrimitiveSubtype::kBool:
    case PrimitiveSubtype::kFloat32:
    case PrimitiveSubtype::kFloat64:
    case PrimitiveSubtype::kZxUsize64:
    case PrimitiveSubtype::kZxUintptr64:
    case PrimitiveSubtype::kZxUchar:
      reporter()->Fail(ErrEnumTypeMustBeIntegralPrimitive, enum_declaration->name.span().value(),
                       enum_declaration->subtype_ctor->type);
      break;
  }
}

void CompileStep::CompileResource(Resource* resource_declaration) {
  CompileAttributeList(resource_declaration->attributes.get());
  CompileTypeConstructor(resource_declaration->subtype_ctor.get());
  if (!resource_declaration->subtype_ctor->type) {
    return;
  }

  if (resource_declaration->subtype_ctor->type->kind != Type::Kind::kPrimitive ||
      static_cast<const PrimitiveType*>(resource_declaration->subtype_ctor->type)->subtype !=
          PrimitiveSubtype::kUint32) {
    reporter()->Fail(ErrResourceMustBeUint32Derived, resource_declaration->name.span().value(),
                     resource_declaration->name);
  }

  for (auto& property : resource_declaration->properties) {
    CompileAttributeList(property.attributes.get());
    CompileTypeConstructor(property.type_ctor.get());
  }

  // All properties have been compiled at this point, so we can reason about their types.
  auto subtype_property = resource_declaration->LookupProperty("subtype");
  if (subtype_property != nullptr) {
    const Type* subtype_type = subtype_property->type_ctor->type;

    // If the |subtype_type is a |nullptr|, we are in a cycle, which means that the |subtype|
    // property could not possibly be an enum declaration.
    if (subtype_type == nullptr || subtype_type->kind != Type::Kind::kIdentifier ||
        static_cast<const IdentifierType*>(subtype_type)->type_decl->kind != Decl::Kind::kEnum) {
      reporter()->Fail(ErrResourceSubtypePropertyMustReferToEnum, subtype_property->name,
                       resource_declaration->name);
    }
  } else {
    reporter()->Fail(ErrResourceMissingSubtypeProperty, resource_declaration->name.span().value(),
                     resource_declaration->name);
  }

  auto rights_property = resource_declaration->LookupProperty("rights");
  if (rights_property != nullptr) {
    const Type* rights_type = rights_property->type_ctor->type;
    const Type* rights_underlying_type = UnderlyingType(rights_type);
    if (!(rights_underlying_type->kind == Type::Kind::kPrimitive &&
          static_cast<const PrimitiveType*>(rights_underlying_type)->subtype ==
              PrimitiveSubtype::kUint32)) {
      reporter()->Fail(ErrResourceRightsPropertyMustReferToBits, rights_property->name,
                       resource_declaration->name);
    }
  }
}

void CompileStep::CompileProtocol(Protocol* protocol_declaration) {
  CompileAttributeList(protocol_declaration->attributes.get());

  MethodScope method_scope;
  auto CheckScopes = [this, &protocol_declaration, &method_scope](const Protocol* protocol,
                                                                  auto Visitor) -> void {
    for (const auto& composed_protocol : protocol->composed_protocols) {
      auto target = composed_protocol.reference.resolved().element();
      if (target->kind != Element::Kind::kProtocol) {
        // No need to report an error here since it was already done by the loop
        // after the definition of CheckScopes (before calling CheckScopes).
        continue;
      }
      auto composed_protocol_declaration = static_cast<const Protocol*>(target);
      auto span = composed_protocol_declaration->name.span();
      ZX_ASSERT(span);
      if (method_scope.protocols.Insert(composed_protocol_declaration, span.value()).ok()) {
        Visitor(composed_protocol_declaration, Visitor);
      } else {
        // Otherwise we have already seen this protocol in
        // the inheritance graph.
      }
    }
    for (const auto& method : protocol->methods) {
      const auto original_name = method.name.data();
      const auto canonical_name = canonicalize(original_name);
      const auto name_result = method_scope.canonical_names.Insert(canonical_name, method.name);
      if (!name_result.ok()) {
        const auto previous_span = name_result.previous_occurrence();
        if (original_name == previous_span.data()) {
          reporter()->Fail(ErrNameCollision, method.name, Element::Kind::kProtocolMethod,
                           original_name, Element::Kind::kProtocolMethod, previous_span);
        } else {
          reporter()->Fail(ErrNameCollisionCanonical, method.name, Element::Kind::kProtocolMethod,
                           original_name, Element::Kind::kProtocolMethod, previous_span.data(),
                           previous_span, canonical_name);
        }
      }
      if (!method.generated_ordinal64) {
        // If a composed method failed to compile, we do not associate have a
        // generated ordinal, and proceeding leads to a segfault. Instead,
        // continue to the next method, without reporting additional errors (the
        // error emitted when compiling the composed method is sufficient).
        continue;
      }
      if (method.generated_ordinal64->value == 0) {
        reporter()->Fail(ErrGeneratedZeroValueOrdinal, method.generated_ordinal64->span());
      }
      auto ordinal_result =
          method_scope.ordinals.Insert(method.generated_ordinal64->value, method.name);
      if (!ordinal_result.ok()) {
        reporter()->Fail(ErrDuplicateMethodOrdinal, method.generated_ordinal64->span(),
                         ordinal_result.previous_occurrence());
      }

      // Add a pointer to this method to the protocol_declarations list.
      bool is_composed = protocol_declaration != protocol;
      protocol_declaration->all_methods.emplace_back(&method, is_composed);
    }
  };

  // Before scope checking can occur, ordinals must be generated for each of the
  // protocol's methods, including those that were composed from transitive
  // child protocols.  This means that child protocols must be compiled prior to
  // this one, or they will not have generated_ordinal64s on their methods, and
  // will fail the scope check.  Also check for duplicate composed protocols.
  Scope<const Protocol*> scope;
  for (const auto& composed_protocol : protocol_declaration->composed_protocols) {
    CompileAttributeList(composed_protocol.attributes.get());
    auto target = composed_protocol.reference.resolved().element();
    auto span = composed_protocol.reference.span();
    if (target->kind != Element::Kind::kProtocol) {
      reporter()->Fail(ErrComposingNonProtocol, span);
      continue;
    }
    auto target_protocol = static_cast<Protocol*>(target);
    auto result = scope.Insert(target_protocol, span);
    if (!result.ok()) {
      reporter()->Fail(ErrProtocolComposedMultipleTimes, span, result.previous_occurrence());
    }
    CompileDecl(target_protocol);
  }
  for (auto& method : protocol_declaration->methods) {
    CompileAttributeList(method.attributes.get());
    auto selector = GetSelector(method.attributes.get(), method.name);
    if (!IsValidIdentifierComponent(selector) && !IsValidFullyQualifiedMethodIdentifier(selector)) {
      reporter()->Fail(
          ErrInvalidSelectorValue,
          method.attributes->Get("selector")->GetArg(AttributeArg::kDefaultAnonymousName)->span);
      continue;
    }
    // TODO(https://fxbug.dev/42157659): Remove.
    auto library_name = library()->name;
    if (library_name.size() == 2 && library_name[0] == "fuchsia" && library_name[1] == "io" &&
        selector.find('/') == std::string::npos) {
      reporter()->Fail(ErrFuchsiaIoExplicitOrdinals, method.name);
      continue;
    }
    method.generated_ordinal64 = std::make_unique<RawOrdinal64>(method_hasher()(
        library_name, protocol_declaration->name.decl_name(), selector, *method.identifier));
  }

  CheckScopes(protocol_declaration, CheckScopes);

  // Ensure that the method's type constructors for request/response payloads actually exist, and
  // are the right kind of layout.
  std::function<void(const SourceSpan&, const Decl*)> CheckPayloadDeclKind =
      [&](const SourceSpan& method_name, const Decl* decl) -> void {
    switch (decl->kind) {
      case Decl::Kind::kStruct: {
        auto empty = static_cast<const Struct*>(decl)->members.empty();
        auto anonymous = decl->name.as_anonymous();
        auto compiler_generated =
            anonymous && anonymous->provenance == Name::Provenance::kGeneratedEmptySuccessStruct;
        if (empty && !compiler_generated) {
          reporter()->Fail(ErrEmptyPayloadStructs, method_name, method_name.data());
        }
        break;
      }
      case Decl::Kind::kTable:
      case Decl::Kind::kUnion: {
        break;
      }
      case Decl::Kind::kAlias: {
        const auto as_alias = static_cast<const Alias*>(decl);
        const Type* aliased_type = as_alias->partial_type_ctor->type;
        switch (aliased_type->kind) {
          case Type::Kind::kIdentifier: {
            const auto as_identifier_type = static_cast<const IdentifierType*>(aliased_type);
            CheckPayloadDeclKind(method_name, as_identifier_type->type_decl);
            break;
          }
          default: {
            reporter()->Fail(ErrInvalidMethodPayloadLayoutClass, method_name, decl->kind);
            break;
          }
        }
        break;
      }
      default: {
        reporter()->Fail(ErrInvalidMethodPayloadLayoutClass, method_name, decl->kind);
        break;
      }
    }
  };

  // Ensure that structs used as message payloads do not have default values.
  auto CheckNoDefaultMembers = [this](const Decl* decl) -> void {
    switch (decl->kind) {
      case Decl::Kind::kStruct: {
        auto struct_decl = static_cast<const Struct*>(decl);
        for (auto& member : struct_decl->members) {
          if (member.maybe_default_value != nullptr) {
            reporter()->Fail(ErrPayloadStructHasDefaultMembers, member.name);
            break;
          }
        }

        break;
      }
      default: {
        return;
      }
    }
  };

  for (auto& method : protocol_declaration->methods) {
    if (method.maybe_request) {
      CompileTypeConstructor(method.maybe_request.get());
      if (auto type = method.maybe_request->type) {
        if (type->kind != Type::Kind::kIdentifier) {
          reporter()->Fail(ErrInvalidMethodPayloadType, method.name, type);
        } else {
          ZX_ASSERT(type->kind == Type::Kind::kIdentifier);
          auto decl = static_cast<const IdentifierType*>(type)->type_decl;
          CompileDecl(decl);
          CheckNoDefaultMembers(decl);
          CheckPayloadDeclKind(method.name, decl);
        }
      }
    }
    if (method.maybe_response) {
      CompileTypeConstructor(method.maybe_response.get());
      if (auto type = method.maybe_response->type) {
        if (type->kind != Type::Kind::kIdentifier) {
          reporter()->Fail(ErrInvalidMethodPayloadType, method.name, type);
        } else {
          ZX_ASSERT(type->kind == Type::Kind::kIdentifier);
          auto decl = static_cast<const IdentifierType*>(type)->type_decl;
          CompileDecl(decl);
          if (method.HasResultUnion()) {
            ZX_ASSERT(decl->kind == Decl::Kind::kUnion);
            const auto* result_union = static_cast<const Union*>(decl);
            ZX_ASSERT(!result_union->members.empty());
            ZX_ASSERT(result_union->members[0].maybe_used);
            const auto* success_variant_type = result_union->members[0].maybe_used->type_ctor->type;
            if (success_variant_type) {
              if (success_variant_type->kind != Type::Kind::kIdentifier) {
                reporter()->Fail(ErrInvalidMethodPayloadType, method.name, success_variant_type);
              } else {
                const auto* success_decl =
                    static_cast<const IdentifierType*>(success_variant_type)->type_decl;
                CheckNoDefaultMembers(success_decl);
                CheckPayloadDeclKind(method.name, success_decl);
              }
            }
            ValidateDomainErrorType(result_union);
          } else {
            CheckNoDefaultMembers(decl);
            CheckPayloadDeclKind(method.name, decl);
          }
        }
      }
    }
  }

  // Ensure that events do not use the error syntax except those in an allowlist.
  // TODO(https://fxbug.dev/42180639): Error syntax in events should not parse.
  auto CheckNoEventErrorSyntax = [this](const Protocol::Method& event) -> void {
    if (!event.maybe_response)
      return;
    if (!event.HasResultUnion())
      return;
    const auto& protocol = *event.owning_protocol;
    const Library& library = *protocol.name.library();
    // TODO(https://fxbug.dev/42180639): Migrate test libraries.
    ZX_ASSERT(!library.name.empty());
    if (library.name[0] == "test" || library.name[0] == "fidl") {
      return;
    }
    // TODO(https://fxbug.dev/42182418): Migrate fuchsia.hardware.radar.
    if (library.name.size() == 3) {
      if (library.name[0] == "fuchsia" && library.name[1] == "hardware" &&
          library.name[2] == "radar") {
        return;
      }
    }
    reporter()->Fail(ErrEventErrorSyntaxDeprecated, event.name, event.name.data());
  };
  for (auto& method : protocol_declaration->methods) {
    if (method.has_response && !method.has_request) {
      CheckNoEventErrorSyntax(method);
    }
  }
}

void CompileStep::ValidateDomainErrorType(const Union* result_union) {
  if (experimental_flags().IsEnabled(ExperimentalFlag::kAllowArbitraryErrorTypes)) {
    return;
  }
  auto& error_member = result_union->members[1];
  if (!error_member.maybe_used) {
    return;
  }
  auto error_type = error_member.maybe_used->type_ctor->type;
  if (!error_type) {
    return;
  }
  const PrimitiveType* error_primitive = nullptr;
  if (error_type->kind == Type::Kind::kPrimitive) {
    error_primitive = static_cast<const PrimitiveType*>(error_type);
  } else if (error_type->kind == Type::Kind::kIdentifier) {
    auto identifier_type = static_cast<const IdentifierType*>(error_type);
    if (identifier_type->type_decl->kind == Decl::Kind::kEnum) {
      auto error_enum = static_cast<const Enum*>(identifier_type->type_decl);
      ZX_ASSERT(error_enum->subtype_ctor->type->kind == Type::Kind::kPrimitive);
      error_primitive = static_cast<const PrimitiveType*>(error_enum->subtype_ctor->type);
    }
  }
  if (!error_primitive || (error_primitive->subtype != PrimitiveSubtype::kInt32 &&
                           error_primitive->subtype != PrimitiveSubtype::kUint32)) {
    reporter()->Fail(ErrInvalidErrorType, result_union->name.span().value());
  }
}

void CompileStep::CompileService(Service* service_decl) {
  std::string_view associated_transport;
  std::string_view first_member_with_that_transport;

  CompileAttributeList(service_decl->attributes.get());
  for (auto& member : service_decl->members) {
    CompileAttributeList(member.attributes.get());
    CompileTypeConstructor(member.type_ctor.get());
    if (!member.type_ctor->type) {
      continue;
    }
    if (member.type_ctor->type->kind != Type::Kind::kTransportSide) {
      reporter()->Fail(ErrOnlyClientEndsInServices, member.name);
      continue;
    }
    const auto transport_side_type = static_cast<const TransportSideType*>(member.type_ctor->type);
    if (transport_side_type->end != TransportSide::kClient) {
      reporter()->Fail(ErrOnlyClientEndsInServices, member.name);
    }
    if (member.type_ctor->type->IsNullable()) {
      reporter()->Fail(ErrOptionalServiceMember, member.name);
    }

    // Enforce that all client_end members are over the same transport.
    // TODO(https://fxbug.dev/42057496): We may need to revisit this restriction.
    if (associated_transport.empty()) {
      associated_transport = transport_side_type->protocol_transport;
      first_member_with_that_transport = member.name.data();
      continue;
    }
    if (associated_transport != transport_side_type->protocol_transport) {
      reporter()->Fail(ErrMismatchedTransportInServices, member.name, member.name.data(),
                       transport_side_type->protocol_transport, first_member_with_that_transport,
                       associated_transport);
    }
  }
}

void CompileStep::CompileStruct(Struct* struct_declaration) {
  CompileAttributeList(struct_declaration->attributes.get());
  for (auto& member : struct_declaration->members) {
    CompileAttributeList(member.attributes.get());
    CompileTypeConstructor(member.type_ctor.get());
    if (!member.type_ctor->type) {
      continue;
    }
    if (member.maybe_default_value) {
      const auto* default_value_type = member.type_ctor->type;
      if (!TypeCanBeConst(default_value_type)) {
        reporter()->Fail(ErrInvalidStructMemberType, struct_declaration->name.span().value(),
                         NameIdentifier(member.name), default_value_type);
      } else if (!ResolveConstant(member.maybe_default_value.get(), default_value_type)) {
        reporter()->Fail(ErrCouldNotResolveMemberDefault, member.name, NameIdentifier(member.name));
      }
    }
  }
}

void CompileStep::CompileTable(Table* table_declaration) {
  Ordinal64Scope ordinal_scope;

  CompileAttributeList(table_declaration->attributes.get());
  for (size_t i = 0; i < table_declaration->members.size(); i++) {
    auto& member = table_declaration->members[i];
    CompileAttributeList(member.attributes.get());
    const auto ordinal_result = ordinal_scope.Insert(member.ordinal->value, member.ordinal->span());
    if (!ordinal_result.ok()) {
      reporter()->Fail(ErrDuplicateTableFieldOrdinal, member.ordinal->span(),
                       ordinal_result.previous_occurrence());
    }
    if (member.ordinal->value > kMaxTableOrdinals) {
      reporter()->Fail(ErrTableOrdinalTooLarge, member.ordinal->span());
    }
    if (!member.maybe_used) {
      continue;
    }
    auto& member_used = *member.maybe_used;
    CompileTypeConstructor(member_used.type_ctor.get());
    if (!member_used.type_ctor->type) {
      continue;
    }
    if (member_used.type_ctor->type->IsNullable()) {
      reporter()->Fail(ErrOptionalTableMember, member_used.name);
    }
    if (i == kMaxTableOrdinals - 1) {
      if (member_used.type_ctor->type->kind != Type::Kind::kIdentifier) {
        reporter()->Fail(ErrMaxOrdinalNotTable, member_used.name);
      } else {
        auto identifier_type = static_cast<const IdentifierType*>(member_used.type_ctor->type);
        if (identifier_type->type_decl->kind != Decl::Kind::kTable) {
          reporter()->Fail(ErrMaxOrdinalNotTable, member_used.name);
        }
      }
    }
  }
}

void CompileStep::CompileUnion(Union* union_declaration) {
  Ordinal64Scope ordinal_scope;

  CompileAttributeList(union_declaration->attributes.get());
  bool contains_non_reserved_member = false;
  bool infer_resourceness = !union_declaration->resourceness.has_value();
  auto resourceness = Resourceness::kValue;
  for (const auto& member : union_declaration->members) {
    CompileAttributeList(member.attributes.get());
    const auto ordinal_result = ordinal_scope.Insert(member.ordinal->value, member.ordinal->span());
    if (!ordinal_result.ok()) {
      reporter()->Fail(ErrDuplicateUnionMemberOrdinal, member.ordinal->span(),
                       ordinal_result.previous_occurrence());
    }
    if (!member.maybe_used) {
      continue;
    }

    contains_non_reserved_member = true;
    const auto& member_used = *member.maybe_used;
    CompileTypeConstructor(member_used.type_ctor.get());
    if (!member_used.type_ctor->type) {
      continue;
    }
    if (member_used.type_ctor->type->IsNullable()) {
      reporter()->Fail(ErrOptionalUnionMember, member_used.name);
    }
    if (infer_resourceness &&
        member_used.type_ctor->type->Resourceness() == Resourceness::kResource) {
      resourceness = Resourceness::kResource;
    }
  }

  if (infer_resourceness) {
    auto* name = union_declaration->name.as_anonymous();
    ZX_ASSERT(name && name->provenance == Name::Provenance::kGeneratedResultUnion);
    union_declaration->resourceness = resourceness;
  }

  if (union_declaration->strictness == Strictness::kStrict && !contains_non_reserved_member) {
    reporter()->Fail(ErrMustHaveOneMember, union_declaration->name.span().value());
  }
}

void CompileStep::CompileOverlay(Overlay* overlay_declaration) {
  if (overlay_declaration->strictness != Strictness::kStrict) {
    reporter()->Fail(ErrOverlayMustBeStrict, overlay_declaration->name.span().value());
  }
  if (overlay_declaration->resourceness == Resourceness::kResource) {
    reporter()->Fail(ErrOverlayMustBeValue, overlay_declaration->name.span().value());
  }
  Ordinal64Scope ordinal_scope;

  CompileAttributeList(overlay_declaration->attributes.get());
  for (const auto& member : overlay_declaration->members) {
    CompileAttributeList(member.attributes.get());
    const auto ordinal_result = ordinal_scope.Insert(member.ordinal->value, member.ordinal->span());
    if (!ordinal_result.ok()) {
      // TODO(https://fxbug.dev/42074906): Consolidate errors for duplicate member ordinals.
      reporter()->Fail(ErrDuplicateUnionMemberOrdinal, member.ordinal->span(),
                       ordinal_result.previous_occurrence());
    }
    if (!member.maybe_used) {
      reporter()->Fail(ErrOverlayMustNotContainReserved, member.span.value());
      continue;
    }
    const auto& member_used = *member.maybe_used;
    CompileTypeConstructor(member_used.type_ctor.get());
    if (!member_used.type_ctor->type) {
      continue;
    }
  }
}

void CompileStep::CompileAlias(Alias* alias) {
  CompileAttributeList(alias->attributes.get());
  CompileTypeConstructor(alias->partial_type_ctor.get());
}

void CompileStep::CompileNewType(NewType* new_type) {
  CompileAttributeList(new_type->attributes.get());
  CompileTypeConstructor(new_type->type_ctor.get());
}

void CompileStep::CompileTypeConstructor(TypeConstructor* type_ctor, bool compile_decls) {
  if (type_ctor->type != nullptr) {
    return;
  }
  TypeResolver type_resolver(this);
  type_ctor->type =
      typespace()->Create(&type_resolver, type_ctor->layout, *type_ctor->parameters,
                          *type_ctor->constraints, compile_decls, &type_ctor->resolved_params);
}

bool CompileStep::ResolveHandleRightsConstant(Resource* resource, Constant* constant,
                                              const HandleRightsValue** out_rights) {
  auto rights_property = resource->LookupProperty("rights");
  if (!rights_property) {
    return false;
  }
  ZX_ASSERT_MSG(rights_property->type_ctor->type, "resource must already be compiled");
  if (!ResolveConstant(constant, rights_property->type_ctor->type)) {
    return false;
  }

  if (out_rights) {
    *out_rights = static_cast<const HandleRightsValue*>(&constant->Value());
  }
  return true;
}

bool CompileStep::ResolveHandleSubtypeIdentifier(Resource* resource, Constant* constant,
                                                 HandleSubtype* out_obj_type) {
  ZX_ASSERT_MSG(resource != nullptr, "must pass resource");

  auto subtype_property = resource->LookupProperty("subtype");
  if (!subtype_property) {
    return false;
  }
  ZX_ASSERT_MSG(subtype_property->type_ctor->type, "resource must already be compiled");
  if (!ResolveConstant(constant, subtype_property->type_ctor->type)) {
    return false;
  }

  if (out_obj_type) {
    auto constant_value = static_cast<const HandleSubtypeValue*>(&constant->Value());
    *out_obj_type = static_cast<HandleSubtype>(constant_value->value);
  }
  return true;
}

bool CompileStep::ResolveSizeBound(Constant* size_constant, const SizeValue** out_size) {
  if (size_constant->kind == Constant::Kind::kIdentifier) {
    auto identifier_constant = static_cast<IdentifierConstant*>(size_constant);
    auto target = identifier_constant->reference.resolved().element();
    if (target->kind == Element::Kind::kBuiltin &&
        static_cast<Builtin*>(target)->id == Builtin::Identity::kMax) {
      size_constant->ResolveTo(SizeValue::Max().Clone(),
                               typespace()->GetPrimitiveType(PrimitiveSubtype::kUint32));
    }
  }
  if (!size_constant->IsResolved()) {
    if (!ResolveConstant(size_constant, typespace()->GetPrimitiveType(PrimitiveSubtype::kUint32))) {
      return false;
    }
  }
  if (out_size) {
    *out_size = static_cast<const SizeValue*>(&size_constant->Value());
  }
  return true;
}

template <typename DeclType, typename MemberType>
bool CompileStep::ValidateMembers(DeclType* decl, MemberValidator<MemberType> validator) {
  ZX_ASSERT(decl != nullptr);
  auto checkpoint = reporter()->Checkpoint();

  Scope<MemberType> value_scope;
  for (const auto& member : decl->members) {
    ZX_ASSERT_MSG(member.value != nullptr, "member value is null");
    if (!ResolveConstant(member.value.get(), decl->subtype_ctor->type)) {
      reporter()->Fail(ErrCouldNotResolveMember, member.name, decl->kind);
      continue;
    }

    MemberType value =
        static_cast<const NumericConstantValue<MemberType>&>(member.value->Value()).value;
    const auto value_result = value_scope.Insert(value, member.name);
    if (!value_result.ok()) {
      const auto previous_span = value_result.previous_occurrence();
      // We can log the error and then continue validating other members for other bugs
      reporter()->Fail(ErrDuplicateMemberValue, member.name, decl->kind, member.name.data(),
                       previous_span.data(), previous_span);
    }

    auto err = validator(value, member.attributes.get(), member.name);
    if (err) {
      reporter()->Report(std::move(err));
    }
  }

  return checkpoint.NoNewErrors();
}

template <typename T>
static bool IsPowerOfTwo(T t) {
  if (t == 0) {
    return false;
  }
  if ((t & (t - 1)) != 0) {
    return false;
  }
  return true;
}

template <typename MemberType>
bool CompileStep::ValidateBitsMembersAndCalcMask(Bits* bits_decl, MemberType* out_mask) {
  static_assert(std::is_unsigned<MemberType>::value && !std::is_same<MemberType, bool>::value,
                "bits members must be an unsigned integral type");
  // Each bits member must be a power of two.
  MemberType mask = 0u;
  auto validator = [&mask](MemberType member, const AttributeList*,
                           SourceSpan span) -> std::unique_ptr<Diagnostic> {
    if (!IsPowerOfTwo(member)) {
      return Diagnostic::MakeError(ErrBitsMemberMustBePowerOfTwo, span);
    }
    mask |= member;
    return nullptr;
  };
  if (!ValidateMembers<Bits, MemberType>(bits_decl, validator)) {
    return false;
  }
  *out_mask = mask;
  return true;
}

template <typename MemberType>
bool CompileStep::ValidateEnumMembersAndCalcUnknownValue(Enum* enum_decl,
                                                         MemberType* out_unknown_value) {
  static_assert(std::is_integral<MemberType>::value && !std::is_same<MemberType, bool>::value,
                "enum members must be an integral type");

  const auto default_unknown_value = std::numeric_limits<MemberType>::max();
  std::optional<MemberType> explicit_unknown_value;
  for (const auto& member : enum_decl->members) {
    if (!ResolveConstant(member.value.get(), enum_decl->subtype_ctor->type)) {
      // ValidateMembers will resolve each member and report errors.
      continue;
    }
    if (member.attributes->Get("unknown") != nullptr) {
      if (explicit_unknown_value.has_value()) {
        return reporter()->Fail(ErrUnknownAttributeOnMultipleEnumMembers, member.name);
      }
      explicit_unknown_value =
          static_cast<const NumericConstantValue<MemberType>&>(member.value->Value()).value;
    }
  }

  auto validator = [enum_decl, &explicit_unknown_value](
                       MemberType member, const AttributeList* attributes,
                       SourceSpan span) -> std::unique_ptr<Diagnostic> {
    switch (enum_decl->strictness) {
      case Strictness::kStrict:
        if (attributes->Get("unknown") != nullptr) {
          return Diagnostic::MakeError(ErrUnknownAttributeOnStrictEnumMember, span);
        }
        return nullptr;
      case Strictness::kFlexible:
        if (member == default_unknown_value && !explicit_unknown_value.has_value()) {
          return Diagnostic::MakeError(ErrFlexibleEnumMemberWithMaxValue, span,
                                       std::to_string(default_unknown_value));
        }
        return nullptr;
    }
  };
  if (!ValidateMembers<Enum, MemberType>(enum_decl, validator)) {
    return false;
  }
  *out_unknown_value = explicit_unknown_value.value_or(default_unknown_value);
  return true;
}

}  // namespace fidlc
