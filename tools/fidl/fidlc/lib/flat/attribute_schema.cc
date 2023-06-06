// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "tools/fidl/fidlc/include/fidl/flat/attribute_schema.h"

#include <zircon/assert.h>

#include "tools/fidl/fidlc/include/fidl/diagnostics.h"
#include "tools/fidl/fidlc/include/fidl/flat/compile_step.h"
#include "tools/fidl/fidlc/include/fidl/flat/transport.h"
#include "tools/fidl/fidlc/include/fidl/flat/typespace.h"
#include "tools/fidl/fidlc/include/fidl/flat_ast.h"

namespace fidl::flat {

AttributeSchema& AttributeSchema::RestrictTo(std::set<Element::Kind> placements) {
  ZX_ASSERT_MSG(!placements.empty(), "must allow some placements");
  ZX_ASSERT_MSG(kind_ == AttributeSchema::Kind::kValidateOnly ||
                    kind_ == AttributeSchema::Kind::kUseEarly ||
                    kind_ == AttributeSchema::Kind::kCompileEarly,
                "wrong kind");
  ZX_ASSERT_MSG(placement_ == AttributeSchema::Placement::kAnywhere, "already set placements");
  ZX_ASSERT_MSG(specific_placements_.empty(), "already set placements");
  placement_ = AttributeSchema::Placement::kSpecific;
  specific_placements_ = std::move(placements);
  return *this;
}

AttributeSchema& AttributeSchema::RestrictToAnonymousLayouts() {
  ZX_ASSERT_MSG(kind_ == AttributeSchema::Kind::kValidateOnly ||
                    kind_ == AttributeSchema::Kind::kUseEarly ||
                    kind_ == AttributeSchema::Kind::kCompileEarly,
                "wrong kind");
  ZX_ASSERT_MSG(placement_ == AttributeSchema::Placement::kAnywhere, "already set placements");
  ZX_ASSERT_MSG(specific_placements_.empty(), "already set placements");
  placement_ = AttributeSchema::Placement::kAnonymousLayout;
  return *this;
}

AttributeSchema& AttributeSchema::DisallowOnAnonymousLayouts() {
  ZX_ASSERT_MSG(kind_ == AttributeSchema::Kind::kValidateOnly ||
                    kind_ == AttributeSchema::Kind::kUseEarly ||
                    kind_ == AttributeSchema::Kind::kCompileEarly,
                "wrong kind");
  ZX_ASSERT_MSG(placement_ == AttributeSchema::Placement::kAnywhere, "already set placements");
  ZX_ASSERT_MSG(specific_placements_.empty(), "already set placements");
  placement_ = AttributeSchema::Placement::kAnythingButAnonymousLayout;
  return *this;
}

AttributeSchema& AttributeSchema::AddArg(AttributeArgSchema arg_schema) {
  ZX_ASSERT_MSG(kind_ == AttributeSchema::Kind::kValidateOnly ||
                    kind_ == AttributeSchema::Kind::kUseEarly ||
                    kind_ == AttributeSchema::Kind::kCompileEarly,
                "wrong kind");
  ZX_ASSERT_MSG(arg_schemas_.empty(), "can only have one unnamed arg");
  arg_schemas_.emplace(AttributeArg::kDefaultAnonymousName, arg_schema);
  return *this;
}

AttributeSchema& AttributeSchema::AddArg(std::string name, AttributeArgSchema arg_schema) {
  ZX_ASSERT_MSG(kind_ == AttributeSchema::Kind::kValidateOnly ||
                    kind_ == AttributeSchema::Kind::kUseEarly ||
                    kind_ == AttributeSchema::Kind::kCompileEarly,
                "wrong kind");
  auto [_, inserted] = arg_schemas_.try_emplace(std::move(name), arg_schema);
  ZX_ASSERT_MSG(inserted, "duplicate argument name");
  return *this;
}

AttributeSchema& AttributeSchema::Constrain(AttributeSchema::Constraint constraint) {
  ZX_ASSERT_MSG(constraint != nullptr, "constraint must be non-null");
  ZX_ASSERT_MSG(constraint_ == nullptr, "already set constraint");
  ZX_ASSERT_MSG(kind_ == AttributeSchema::Kind::kValidateOnly,
                "constraints only allowed on kValidateOnly attributes");
  constraint_ = std::move(constraint);
  return *this;
}

AttributeSchema& AttributeSchema::UseEarly() {
  ZX_ASSERT_MSG(kind_ == AttributeSchema::Kind::kValidateOnly, "already changed kind");
  ZX_ASSERT_MSG(constraint_ == nullptr, "use-early attribute should not specify constraint");
  kind_ = AttributeSchema::Kind::kUseEarly;
  return *this;
}

AttributeSchema& AttributeSchema::CompileEarly() {
  ZX_ASSERT_MSG(kind_ == AttributeSchema::Kind::kValidateOnly, "already changed kind");
  ZX_ASSERT_MSG(constraint_ == nullptr, "compile-early attribute should not specify constraint");
  kind_ = AttributeSchema::Kind::kCompileEarly;
  return *this;
}

AttributeSchema& AttributeSchema::Deprecate() {
  ZX_ASSERT_MSG(kind_ == AttributeSchema::Kind::kValidateOnly, "wrong kind");
  ZX_ASSERT_MSG(placement_ == AttributeSchema::Placement::kAnywhere,
                "deprecated attribute should not specify placement");
  ZX_ASSERT_MSG(arg_schemas_.empty(), "deprecated attribute should not specify arguments");
  ZX_ASSERT_MSG(constraint_ == nullptr, "deprecated attribute should not specify constraint");
  kind_ = AttributeSchema::Kind::kDeprecated;
  return *this;
}

// static
const AttributeSchema AttributeSchema::kUserDefined(Kind::kUserDefined);

void AttributeSchema::Validate(Reporter* reporter, const ExperimentalFlags flags,
                               const Attribute* attribute, const Element* element) const {
  switch (kind_) {
    case Kind::kValidateOnly:
      break;
    case Kind::kUseEarly:
    case Kind::kCompileEarly:
      ZX_ASSERT_MSG(constraint_ == nullptr,
                    "use-early and compile-early schemas should not have a constraint");
      break;
    case Kind::kDeprecated:
      reporter->Fail(ErrDeprecatedAttribute, attribute->span, attribute);
      return;
    case Kind::kUserDefined:
      return;
  }

  bool valid_placement;
  switch (placement_) {
    case Placement::kAnywhere:
      valid_placement = true;
      break;
    case Placement::kSpecific:
      valid_placement = specific_placements_.count(element->kind) > 0;
      break;
    case Placement::kAnonymousLayout:
      valid_placement = element->IsAnonymousLayout();
      break;
    case Placement::kAnythingButAnonymousLayout:
      valid_placement = !element->IsAnonymousLayout();
      break;
  }
  if (!valid_placement) {
    reporter->Fail(ErrInvalidAttributePlacement, attribute->span, attribute);
    return;
  }

  if (constraint_ == nullptr) {
    return;
  }
  auto check = reporter->Checkpoint();
  auto passed = constraint_(reporter, flags, attribute, element);
  if (passed) {
    ZX_ASSERT_MSG(check.NoNewErrors(), "cannot add errors and pass");
    return;
  }
  ZX_ASSERT_MSG(!check.NoNewErrors(), "cannot fail a constraint without reporting errors");
}

void AttributeSchema::ResolveArgs(CompileStep* step, Attribute* attribute) const {
  switch (kind_) {
    case Kind::kValidateOnly:
    case Kind::kUseEarly:
    case Kind::kCompileEarly:
      break;
    case Kind::kDeprecated:
      // Don't attempt to resolve arguments, as we don't store argument schemas
      // for deprecated attributes. Instead, rely on AttributeSchema::Validate
      // to report the error.
      return;
    case Kind::kUserDefined:
      ResolveArgsWithoutSchema(step, attribute);
      return;
  }

  // Name the anonymous argument (if present).
  if (auto anon_arg = attribute->GetStandaloneAnonymousArg()) {
    if (arg_schemas_.empty()) {
      step->Fail(ErrAttributeDisallowsArgs, attribute->span, attribute);
      return;
    }
    if (arg_schemas_.size() > 1) {
      step->Fail(ErrAttributeArgNotNamed, attribute->span, anon_arg->value->span.data());
      return;
    }
    anon_arg->name = step->generated_source_file()->AddLine(arg_schemas_.begin()->first);
  } else if (arg_schemas_.size() == 1 && attribute->args.size() == 1) {
    step->Fail(ErrAttributeArgMustNotBeNamed, attribute->span);
  }

  // Resolve each argument by name.
  for (auto& arg : attribute->args) {
    const auto it = arg_schemas_.find(arg->name.value().data());
    if (it == arg_schemas_.end()) {
      step->Fail(ErrUnknownAttributeArg, attribute->span, attribute, arg->name.value().data());
      continue;
    }
    const auto& [name, schema] = *it;
    const bool literal_only = kind_ == Kind::kCompileEarly;
    schema.ResolveArg(step, attribute, arg.get(), literal_only);
  }

  // Check for missing arguments.
  for (const auto& [name, schema] : arg_schemas_) {
    if (schema.IsOptional() || attribute->GetArg(name) != nullptr) {
      continue;
    }
    if (arg_schemas_.size() == 1) {
      step->Fail(ErrMissingRequiredAnonymousAttributeArg, attribute->span, attribute);
    } else {
      step->Fail(ErrMissingRequiredAttributeArg, attribute->span, attribute, name);
    }
  }
}

static bool RefersToHead(const std::vector<std::string_view>& components, const Decl* head_decl) {
  auto head_name = head_decl->name.decl_name();
  if (components.size() == 1 && components[0] == head_name) {
    return true;
  }
  auto& library_name = head_decl->name.library()->name;
  return components.size() == library_name.size() + 1 &&
         std::equal(library_name.begin(), library_name.end(), components.begin()) &&
         components.back() == head_name;
}

bool AttributeArgSchema::TryResolveAsHead(CompileStep* step, Reference& reference) const {
  Decl* head_decl =
      step->all_libraries()->root_library()->declarations.LookupBuiltin(Builtin::Identity::kHead);
  switch (reference.state()) {
    // Usually the reference will be kRawSourced because we are coming here from
    // the AvailabilityStep via CompileStep::CompileAttributeEarly (i.e. before
    // the ResolveStep so nothing is resolved yet).
    case Reference::State::kRawSourced:
      if (RefersToHead(reference.raw_sourced().components, head_decl)) {
        auto name = head_decl->name;
        reference.SetKey(Reference::Key(name.library(), name.decl_name()));
        reference.ResolveTo(Reference::Target(head_decl));
        return true;
      }
      return false;
    // However, there is one scenario where the reference is already resolved:
    //
    // * The @available attribute occurs (incorrectly) on the library
    //   declaration in two of the library's .fidl files.
    // * The AvailabilityStep uses attributes->Get("available"), which just
    //   returns the first one, and compiles it early.
    // * The second one, e.g. @available(added=HEAD), gets resolved and compiled
    //   as normal, so it's already resolved at this point.
    //
    // In this case the CompileStep will fail with ErrDuplicateAttribute soon
    // after returning from here.
    case Reference::State::kResolved:
      return reference.resolved().element() == head_decl;
    default:
      ZX_PANIC("unexpected reference state");
  }
}

void AttributeArgSchema::ResolveArg(CompileStep* step, Attribute* attribute, AttributeArg* arg,
                                    bool literal_only) const {
  Constant* constant = arg->value.get();
  ZX_ASSERT_MSG(!constant->IsResolved(), "argument should not be resolved yet");

  ConstantValue::Kind kind;
  if (auto special_case = std::get_if<SpecialCase>(&type_)) {
    ZX_ASSERT_MSG(*special_case == SpecialCase::kVersion, "unhandled special case");
    kind = ConstantValue::Kind::kUint64;
    if (constant->kind == Constant::Kind::kIdentifier) {
      if (TryResolveAsHead(step, static_cast<IdentifierConstant*>(constant)->reference)) {
        constant->ResolveTo(
            std::make_unique<NumericConstantValue<uint64_t>>(Version::Head().ordinal()),
            step->typespace()->GetPrimitiveType(types::PrimitiveSubtype::kUint64));
        return;
      }
    }
  } else {
    kind = std::get<ConstantValue::Kind>(type_);
  }

  if (literal_only && constant->kind != Constant::Kind::kLiteral) {
    step->Fail(ErrAttributeArgRequiresLiteral, constant->span, arg->name.value().data(), attribute);
    return;
  }

  const Type* target_type;
  switch (kind) {
    case ConstantValue::Kind::kDocComment:
      ZX_PANIC("we know the target type of doc comments, and should not end up here");
    case ConstantValue::Kind::kString:
      target_type = step->typespace()->GetUnboundedStringType();
      break;
    case ConstantValue::Kind::kBool:
      target_type = step->typespace()->GetPrimitiveType(types::PrimitiveSubtype::kBool);
      break;
    case ConstantValue::Kind::kInt8:
      target_type = step->typespace()->GetPrimitiveType(types::PrimitiveSubtype::kInt8);
      break;
    case ConstantValue::Kind::kInt16:
      target_type = step->typespace()->GetPrimitiveType(types::PrimitiveSubtype::kInt16);
      break;
    case ConstantValue::Kind::kInt32:
      target_type = step->typespace()->GetPrimitiveType(types::PrimitiveSubtype::kInt32);
      break;
    case ConstantValue::Kind::kInt64:
      target_type = step->typespace()->GetPrimitiveType(types::PrimitiveSubtype::kInt64);
      break;
    case ConstantValue::Kind::kUint8:
      target_type = step->typespace()->GetPrimitiveType(types::PrimitiveSubtype::kUint8);
      break;
    case ConstantValue::Kind::kZxUchar:
      target_type = step->typespace()->GetPrimitiveType(types::PrimitiveSubtype::kZxUchar);
      break;
    case ConstantValue::Kind::kUint16:
      target_type = step->typespace()->GetPrimitiveType(types::PrimitiveSubtype::kUint16);
      break;
    case ConstantValue::Kind::kUint32:
      target_type = step->typespace()->GetPrimitiveType(types::PrimitiveSubtype::kUint32);
      break;
    case ConstantValue::Kind::kUint64:
      target_type = step->typespace()->GetPrimitiveType(types::PrimitiveSubtype::kUint64);
      break;
    case ConstantValue::Kind::kZxUsize64:
      target_type = step->typespace()->GetPrimitiveType(types::PrimitiveSubtype::kZxUsize64);
      break;
    case ConstantValue::Kind::kZxUintptr64:
      target_type = step->typespace()->GetPrimitiveType(types::PrimitiveSubtype::kZxUintptr64);
      break;
    case ConstantValue::Kind::kFloat32:
      target_type = step->typespace()->GetPrimitiveType(types::PrimitiveSubtype::kFloat32);
      break;
    case ConstantValue::Kind::kFloat64:
      target_type = step->typespace()->GetPrimitiveType(types::PrimitiveSubtype::kFloat64);
      break;
  }
  if (!step->ResolveConstant(constant, target_type)) {
    step->Fail(ErrCouldNotResolveAttributeArg, arg->span);
  }
}

// static
void AttributeSchema::ResolveArgsWithoutSchema(CompileStep* step, Attribute* attribute) {
  // For attributes with a single, anonymous argument like `@foo("bar")`, assign
  // a default name so that arguments are always named after compilation.
  if (auto anon_arg = attribute->GetStandaloneAnonymousArg()) {
    anon_arg->name = step->generated_source_file()->AddLine(AttributeArg::kDefaultAnonymousName);
  }

  // Try resolving each argument as string or bool. We don't allow numerics
  // because it's not clear what type (int8, uint32, etc.) we should infer.
  for (const auto& arg : attribute->args) {
    ZX_ASSERT_MSG(arg->value->kind != Constant::Kind::kBinaryOperator,
                  "attribute arg with a binary operator is a parse error");

    auto inferred_type = step->InferType(arg->value.get());
    if (!inferred_type) {
      step->Fail(ErrCouldNotResolveAttributeArg, attribute->span);
      continue;
    }
    // Only string or bool supported.
    switch (inferred_type->kind) {
      case Type::Kind::kString:
        break;
      case Type::Kind::kPrimitive:
        if (static_cast<const PrimitiveType*>(inferred_type)->subtype ==
            types::PrimitiveSubtype::kBool) {
          break;
        }
        [[fallthrough]];
      case Type::Kind::kInternal:
      case Type::Kind::kIdentifier:
      case Type::Kind::kArray:
      case Type::Kind::kBox:
      case Type::Kind::kVector:
      case Type::Kind::kZxExperimentalPointer:
      case Type::Kind::kHandle:
      case Type::Kind::kTransportSide:
      case Type::Kind::kUntypedNumeric:
        step->Fail(ErrCanOnlyUseStringOrBool, attribute->span, arg.get(), attribute);
        continue;
    }
    ZX_ASSERT_MSG(step->ResolveConstant(arg->value.get(), inferred_type),
                  "resolving cannot fail when we've inferred the type");
  }
}

// TODO(fxbug.dev/100478): Remove once large messages no longer requires flag or attribute.
static bool ExperimentalOverflowingConstraint(Reporter* reporter, const ExperimentalFlags flags,
                                              const Attribute* attr, const Element* element) {
  if (!flags.IsFlagEnabled(ExperimentalFlags::Flag::kAllowOverflowing)) {
    return reporter->Fail(ErrExperimentalOverflowingAttributeMissingExperimentalFlag, attr->span);
  }

  bool at_least_one_arg_is_true = false;
  for (const auto& arg : attr->args) {
    if (static_cast<const BoolConstantValue*>(&arg->value->Value())->value) {
      at_least_one_arg_is_true = true;
    }
  }
  if (!at_least_one_arg_is_true) {
    return reporter->Fail(ErrExperimentalOverflowingIncorrectUsage, attr->span);
  }
  return true;
}

static bool DiscoverableConstraint(Reporter* reporter, const ExperimentalFlags flags,
                                   const Attribute* attr, const Element* element) {
  auto arg = attr->GetArg(AttributeArg::kDefaultAnonymousName);
  if (!arg) {
    return true;
  }
  ZX_ASSERT(arg->value->Value().kind == flat::ConstantValue::Kind::kString);
  auto name = static_cast<const flat::StringConstantValue&>(arg->value->Value()).MakeContents();
  if (!utils::IsValidDiscoverableName(name)) {
    return reporter->Fail(ErrInvalidDiscoverableName, arg->span, name);
  }
  return true;
}

static bool ResultShapeConstraint(Reporter* reporter, const ExperimentalFlags flags,
                                  const Attribute* attribute, const Element* element) {
  ZX_ASSERT(element);
  ZX_ASSERT(element->kind == Element::Kind::kUnion);
  auto union_decl = static_cast<const Union*>(element);
  ZX_ASSERT(union_decl->members.size() == 2 || union_decl->members.size() == 3);
  auto& error_member = union_decl->members.at(1);
  ZX_ASSERT_MSG(union_decl->members.size() == 3 || error_member.maybe_used != nullptr,
                "must have an error variant if transport error not used");

  if (error_member.maybe_used != nullptr) {
    auto error_type = error_member.maybe_used->type_ctor->type;
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

    if (!error_primitive || (error_primitive->subtype != types::PrimitiveSubtype::kInt32 &&
                             error_primitive->subtype != types::PrimitiveSubtype::kUint32)) {
      return reporter->Fail(ErrInvalidErrorType, union_decl->name.span().value());
    }
  }

  return true;
}

static bool TransportConstraint(Reporter* reporter, const ExperimentalFlags flags,
                                const Attribute* attribute, const Element* element) {
  ZX_ASSERT(element);
  ZX_ASSERT(element->kind == Element::Kind::kProtocol);

  auto arg = attribute->GetArg(AttributeArg::kDefaultAnonymousName);
  auto& arg_value = static_cast<const flat::StringConstantValue&>(arg->value->Value());

  const std::string& value = arg_value.MakeContents();
  std::optional<Transport> transport = Transport::FromTransportName(value);
  if (!transport.has_value()) {
    return reporter->Fail(ErrInvalidTransportType, attribute->span, value,
                          Transport::AllTransportNames());
  }
  return true;
}

// static
AttributeSchemaMap AttributeSchema::OfficialAttributes() {
  AttributeSchemaMap map;
  // This attribute exists only to demonstrate and test our ability to deprecate
  // attributes. It will never be removed.
  map["example_deprecated_attribute"].Deprecate();
  map["discoverable"]
      .RestrictTo({
          Element::Kind::kProtocol,
      })
      .AddArg(AttributeArgSchema(ConstantValue::Kind::kString,
                                 AttributeArgSchema::Optionality::kOptional))
      .Constrain(DiscoverableConstraint);
  map[std::string(Attribute::kDocCommentName)].AddArg(
      AttributeArgSchema(ConstantValue::Kind::kString));
  // TODO(fxbug.dev/100478): Remove once large messages no longer requires flag or attribute.
  map["experimental_overflowing"]
      .RestrictTo({
          Element::Kind::kProtocolMethod,
      })
      .AddArg("request", AttributeArgSchema(ConstantValue::Kind::kBool,
                                            AttributeArgSchema::Optionality::kOptional))
      .AddArg("response", AttributeArgSchema(ConstantValue::Kind::kBool,
                                             AttributeArgSchema::Optionality::kOptional))
      .Constrain(ExperimentalOverflowingConstraint);
  map["generated_name"]
      .RestrictToAnonymousLayouts()
      .AddArg(AttributeArgSchema(ConstantValue::Kind::kString))
      .CompileEarly();
  map["result"]
      .RestrictTo({
          Element::Kind::kUnion,
      })
      .Constrain(ResultShapeConstraint);
  map["selector"]
      .RestrictTo({
          Element::Kind::kProtocolMethod,
      })
      .AddArg(AttributeArgSchema(ConstantValue::Kind::kString))
      .UseEarly();
  map["transitional"]
      .RestrictTo({
          Element::Kind::kProtocolMethod,
      })
      .AddArg(AttributeArgSchema(ConstantValue::Kind::kString,
                                 AttributeArgSchema::Optionality::kOptional));
  map["transport"]
      .RestrictTo({
          Element::Kind::kProtocol,
      })
      .AddArg(AttributeArgSchema(ConstantValue::Kind::kString))
      .Constrain(TransportConstraint);
  map["unknown"].RestrictTo({Element::Kind::kEnumMember});
  map["available"]
      .DisallowOnAnonymousLayouts()
      .AddArg("platform", AttributeArgSchema(ConstantValue::Kind::kString,
                                             AttributeArgSchema::Optionality::kOptional))
      .AddArg("added", AttributeArgSchema(AttributeArgSchema::SpecialCase::kVersion,
                                          AttributeArgSchema::Optionality::kOptional))
      .AddArg("deprecated", AttributeArgSchema(AttributeArgSchema::SpecialCase::kVersion,
                                               AttributeArgSchema::Optionality::kOptional))
      .AddArg("removed", AttributeArgSchema(AttributeArgSchema::SpecialCase::kVersion,
                                            AttributeArgSchema::Optionality::kOptional))
      .AddArg("note", AttributeArgSchema(ConstantValue::Kind::kString,
                                         AttributeArgSchema::Optionality::kOptional))
      .AddArg("legacy", AttributeArgSchema(ConstantValue::Kind::kBool,
                                           AttributeArgSchema::Optionality::kOptional))
      .CompileEarly();
  return map;
}

}  // namespace fidl::flat
