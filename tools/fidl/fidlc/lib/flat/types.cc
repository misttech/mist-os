// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "tools/fidl/fidlc/include/fidl/flat/types.h"

#include <zircon/assert.h>

#include "tools/fidl/fidlc/include/fidl/diagnostics.h"
#include "tools/fidl/fidlc/include/fidl/flat/type_resolver.h"
#include "tools/fidl/fidlc/include/fidl/flat/visitor.h"
#include "tools/fidl/fidlc/include/fidl/flat_ast.h"

namespace fidl::flat {

// ZX_HANDLE_SAME_RIGHTS
const HandleRights HandleType::kSameRights = HandleRights(0x80000000);

bool RejectOptionalConstraints::OnUnexpectedConstraint(
    TypeResolver* resolver, std::optional<SourceSpan> params_span, const Name& layout_name,
    Resource* resource, size_t num_constraints,
    const std::vector<std::unique_ptr<Constant>>& params, size_t param_index) const {
  if (params.size() == 1 && resolver->ResolveAsOptional(params[0].get())) {
    return resolver->Fail(ErrCannotBeOptional, params[0]->span, layout_name);
  }
  return ConstraintsBase::OnUnexpectedConstraint(resolver, params_span, layout_name, resource,
                                                 num_constraints, params, param_index);
}

bool ArrayConstraints::OnUnexpectedConstraint(TypeResolver* resolver,
                                              std::optional<SourceSpan> params_span,
                                              const Name& layout_name, Resource* resource,
                                              size_t num_constraints,
                                              const std::vector<std::unique_ptr<Constant>>& params,
                                              size_t param_index) const {
  if (params.size() == 1 && resolver->ResolveAsOptional(params[0].get())) {
    return resolver->Fail(ErrCannotBeOptional, params[0]->span, layout_name);
  }
  return ConstraintsBase::OnUnexpectedConstraint(resolver, params_span, layout_name, resource,
                                                 num_constraints, params, param_index);
}

bool ArrayType::ApplyConstraints(TypeResolver* resolver, const TypeConstraints& constraints,
                                 const Reference& layout, std::unique_ptr<Type>* out_type,
                                 LayoutInvocation* out_params) const {
  Constraints c;
  if (!ResolveAndMergeConstraints(resolver, constraints.span, layout.resolved().name(), nullptr,
                                  constraints.items, &c, out_params)) {
    return false;
  }

  if (c.utf8 && !resolver->experimental_flags().IsFlagEnabled(ExperimentalFlags::Flag::kZxCTypes)) {
    return resolver->Fail(ErrExperimentalZxCTypesDisallowed, layout.span(),
                          layout.resolved().name());
  }

  *out_type = std::make_unique<ArrayType>(name, element_type, element_count, std::move(c));
  return true;
}

bool VectorConstraints::OnUnexpectedConstraint(TypeResolver* resolver,
                                               std::optional<SourceSpan> params_span,
                                               const Name& layout_name, Resource* resource,
                                               size_t num_constraints,
                                               const std::vector<std::unique_ptr<Constant>>& params,
                                               size_t param_index) const {
  if (!params.empty() && param_index == 0) {
    return resolver->Fail(ErrCouldNotResolveSizeBound, params[0]->span);
  }
  return ConstraintsBase::OnUnexpectedConstraint(resolver, params_span, layout_name, resource,
                                                 num_constraints, params, param_index);
}

bool VectorType::ApplyConstraints(TypeResolver* resolver, const TypeConstraints& constraints,
                                  const Reference& layout, std::unique_ptr<Type>* out_type,
                                  LayoutInvocation* out_params) const {
  Constraints c;
  if (!ResolveAndMergeConstraints(resolver, constraints.span, layout.resolved().name(), nullptr,
                                  constraints.items, &c, out_params)) {
    return false;
  }

  *out_type = std::make_unique<VectorType>(name, element_type, std::move(c));
  return true;
}

bool StringType::ApplyConstraints(TypeResolver* resolver, const TypeConstraints& constraints,
                                  const Reference& layout, std::unique_ptr<Type>* out_type,
                                  LayoutInvocation* out_params) const {
  Constraints c;
  if (!ResolveAndMergeConstraints(resolver, constraints.span, layout.resolved().name(), nullptr,
                                  constraints.items, &c, out_params)) {
    return false;
  }

  *out_type = std::make_unique<StringType>(name, c);

  return true;
}

bool HandleType::ApplyConstraints(TypeResolver* resolver, const TypeConstraints& constraints,
                                  const Reference& layout, std::unique_ptr<Type>* out_type,
                                  LayoutInvocation* out_params) const {
  ZX_ASSERT(resource_decl);

  Constraints c;
  if (!ResolveAndMergeConstraints(resolver, constraints.span, layout.resolved().name(),
                                  resource_decl, constraints.items, &c, out_params)) {
    return false;
  }

  *out_type = std::make_unique<HandleType>(name, resource_decl, std::move(c));
  return true;
}

bool TransportSideConstraints::OnUnexpectedConstraint(
    TypeResolver* resolver, std::optional<SourceSpan> params_span, const Name& layout_name,
    Resource* resource, size_t num_constraints,
    const std::vector<std::unique_ptr<Constant>>& params, size_t param_index) const {
  if (!params.empty() && param_index == 0) {
    return resolver->Fail(ErrMustBeAProtocol, params[0]->span, layout_name);
  }
  return ConstraintsBase::OnUnexpectedConstraint(resolver, params_span, layout_name, resource,
                                                 num_constraints, params, param_index);
}

bool TransportSideType::ApplyConstraints(TypeResolver* resolver, const TypeConstraints& constraints,
                                         const Reference& layout, std::unique_ptr<Type>* out_type,
                                         LayoutInvocation* out_params) const {
  Constraints c;
  if (!ResolveAndMergeConstraints(resolver, constraints.span, layout.resolved().name(), nullptr,
                                  constraints.items, &c, out_params)) {
    return false;
  }

  if (!c.HasConstraint<ConstraintKind::kProtocol>()) {
    return resolver->Fail(ErrProtocolConstraintRequired, layout.span(), layout.resolved().name());
  }

  const Attribute* transport_attribute = c.protocol_decl->attributes->Get("transport");
  std::string_view transport("Channel");
  if (transport_attribute) {
    auto arg = (transport_attribute->compiled)
                   ? transport_attribute->GetArg(AttributeArg::kDefaultAnonymousName)
                   : transport_attribute->GetStandaloneAnonymousArg();
    std::string_view quoted_transport =
        static_cast<const LiteralConstant*>(arg->value.get())->literal->span().data();
    // Remove quotes around the transport.
    transport = quoted_transport.substr(1, quoted_transport.size() - 2);
  }

  *out_type = std::make_unique<TransportSideType>(name, std::move(c), end, transport);
  return true;
}

IdentifierType::IdentifierType(TypeDecl* type_decl, Constraints constraints)
    : Type(type_decl->name, Kind::kIdentifier),
      Constraints(std::move(constraints)),
      type_decl(type_decl) {}

bool IdentifierType::ApplyConstraints(TypeResolver* resolver, const TypeConstraints& constraints,
                                      const Reference& layout, std::unique_ptr<Type>* out_type,
                                      LayoutInvocation* out_params) const {
  const auto& layout_name = layout.resolved().name();

  if (type_decl->kind == Decl::Kind::kNewType && !constraints.items.empty()) {
    // Currently, we are disallowing optional on new-types. And since new-types are semi-opaque
    // wrappers around types, we need not bother about other constraints. So no constraints for
    // you, new-types!
    return resolver->Fail(ErrNewTypeCannotHaveConstraint, constraints.span.value(), layout_name);
  }

  Constraints c;
  if (!ResolveAndMergeConstraints(resolver, constraints.span, layout.resolved().name(), nullptr,
                                  constraints.items, &c, out_params)) {
    return false;
  }

  switch (type_decl->kind) {
    // These types have no allowed constraints
    case Decl::Kind::kBits:
    case Decl::Kind::kEnum:
    case Decl::Kind::kTable:
    case Decl::Kind::kOverlay:
      if (c.HasConstraint<ConstraintKind::kNullability>()) {
        return resolver->Fail(ErrCannotBeOptional, constraints.span.value(), layout_name);
      }
      break;

    case Decl::Kind::kStruct:
      if (resolver->experimental_flags().IsFlagEnabled(
              ExperimentalFlags::Flag::kNoOptionalStructs)) {
        // Structs are nullable in the sense that they can be boxed. But we are
        // disallowing optional to be used on struct.
        if (c.HasConstraint<ConstraintKind::kNullability>()) {
          return resolver->Fail(ErrStructCannotBeOptional, constraints.span.value(), layout_name);
        }
      }
      break;

    // These types have one allowed constraint (`optional`). For aliases,
    // we need to allow the possibility that the concrete type does allow `optional`,
    // if it doesn't the Type itself will catch the error.
    case Decl::Kind::kAlias:
    case Decl::Kind::kUnion:
      break;

    case Decl::Kind::kNewType:
      // Constraints on new-types should be handled above.
      ZX_ASSERT(constraints.items.empty());
      break;

    case Decl::Kind::kBuiltin:
    case Decl::Kind::kConst:
    case Decl::Kind::kResource:
      // Cannot have const: entries for constants do not exist in the typespace, so
      // they're caught earlier.
      // Cannot have resource: resource types should have resolved to the HandleTypeTemplate
      ZX_PANIC("unexpected identifier type decl kind");
    // These can't be used as types. This will be caught later, in VerifyTypeCategory.
    case Decl::Kind::kService:
    case Decl::Kind::kProtocol:
      break;
  }

  *out_type = std::make_unique<IdentifierType>(type_decl, std::move(c));
  return true;
}

bool BoxConstraints::OnUnexpectedConstraint(TypeResolver* resolver,
                                            std::optional<SourceSpan> params_span,
                                            const Name& layout_name, Resource* resource,
                                            size_t num_constraints,
                                            const std::vector<std::unique_ptr<Constant>>& params,
                                            size_t param_index) const {
  if (params.size() == 1 && resolver->ResolveAsOptional(params[0].get())) {
    return resolver->Fail(ErrBoxCannotBeOptional, params[0]->span);
  }
  return ConstraintsBase::OnUnexpectedConstraint(resolver, params_span, layout_name, resource,
                                                 num_constraints, params, param_index);
}
bool BoxType::ApplyConstraints(TypeResolver* resolver, const TypeConstraints& constraints,
                               const Reference& layout, std::unique_ptr<Type>* out_type,
                               LayoutInvocation* out_params) const {
  if (!ResolveAndMergeConstraints(resolver, constraints.span, layout.resolved().name(), nullptr,
                                  constraints.items, nullptr)) {
    return false;
  }

  *out_type = std::make_unique<BoxType>(name, boxed_type);
  return true;
}

bool UntypedNumericType::ApplyConstraints(TypeResolver* resolver,
                                          const TypeConstraints& constraints,
                                          const Reference& layout, std::unique_ptr<Type>* out_type,
                                          LayoutInvocation* out_params) const {
  ZX_PANIC("should not have untyped numeric here");
}

uint32_t PrimitiveType::SubtypeSize(types::PrimitiveSubtype subtype) {
  switch (subtype) {
    case types::PrimitiveSubtype::kBool:
    case types::PrimitiveSubtype::kInt8:
    case types::PrimitiveSubtype::kUint8:
    case types::PrimitiveSubtype::kZxUchar:
      return 1u;

    case types::PrimitiveSubtype::kInt16:
    case types::PrimitiveSubtype::kUint16:
      return 2u;

    case types::PrimitiveSubtype::kFloat32:
    case types::PrimitiveSubtype::kInt32:
    case types::PrimitiveSubtype::kUint32:
      return 4u;

    case types::PrimitiveSubtype::kFloat64:
    case types::PrimitiveSubtype::kInt64:
    case types::PrimitiveSubtype::kUint64:
    case types::PrimitiveSubtype::kZxUsize64:
    case types::PrimitiveSubtype::kZxUintptr64:
      return 8u;
  }
}

bool PrimitiveType::ApplyConstraints(TypeResolver* resolver, const TypeConstraints& constraints,
                                     const Reference& layout, std::unique_ptr<Type>* out_type,
                                     LayoutInvocation* out_params) const {
  if (!ResolveAndMergeConstraints(resolver, constraints.span, layout.resolved().name(), nullptr,
                                  constraints.items, nullptr)) {
    return false;
  }

  if ((subtype == types::PrimitiveSubtype::kZxUsize64 ||
       subtype == types::PrimitiveSubtype::kZxUintptr64 ||
       subtype == types::PrimitiveSubtype::kZxUchar) &&
      !resolver->experimental_flags().IsFlagEnabled(ExperimentalFlags::Flag::kZxCTypes)) {
    return resolver->Fail(ErrExperimentalZxCTypesDisallowed, layout.span(),
                          layout.resolved().name());
  }
  *out_type = std::make_unique<PrimitiveType>(name, subtype);
  return true;
}

bool InternalType::ApplyConstraints(TypeResolver* resolver, const TypeConstraints& constraints,
                                    const Reference& layout, std::unique_ptr<Type>* out_type,
                                    LayoutInvocation* out_params) const {
  if (!ResolveAndMergeConstraints(resolver, constraints.span, layout.resolved().name(), nullptr,
                                  constraints.items, nullptr)) {
    return false;
  }
  *out_type = std::make_unique<InternalType>(name, subtype);
  return true;
}

bool ZxExperimentalPointerType::ApplyConstraints(TypeResolver* resolver,
                                                 const TypeConstraints& constraints,
                                                 const Reference& layout,
                                                 std::unique_ptr<Type>* out_type,
                                                 LayoutInvocation* out_params) const {
  if (!ResolveAndMergeConstraints(resolver, constraints.span, layout.resolved().name(), nullptr,
                                  constraints.items, nullptr)) {
    return false;
  }
  if (!resolver->experimental_flags().IsFlagEnabled(ExperimentalFlags::Flag::kZxCTypes)) {
    return resolver->Fail(ErrExperimentalZxCTypesDisallowed, layout.span(),
                          layout.resolved().name());
  }
  *out_type = std::make_unique<ZxExperimentalPointerType>(name, pointee_type);
  return true;
}

types::Resourceness Type::Resourceness() const {
  switch (this->kind) {
    case Type::Kind::kPrimitive:
    case Type::Kind::kInternal:
    case Type::Kind::kString:
      return types::Resourceness::kValue;
    case Type::Kind::kHandle:
    case Type::Kind::kTransportSide:
      return types::Resourceness::kResource;
    case Type::Kind::kArray:
      return static_cast<const ArrayType*>(this)->element_type->Resourceness();
    case Type::Kind::kVector:
      return static_cast<const VectorType*>(this)->element_type->Resourceness();
    case Type::Kind::kZxExperimentalPointer:
      return static_cast<const ZxExperimentalPointerType*>(this)->pointee_type->Resourceness();
    case Type::Kind::kIdentifier:
      break;
    case Type::Kind::kBox:
      return static_cast<const BoxType*>(this)->boxed_type->Resourceness();
    case Type::Kind::kUntypedNumeric:
      ZX_PANIC("should not have untyped numeric here");
  }

  const auto* decl = static_cast<const IdentifierType*>(this)->type_decl;

  switch (decl->kind) {
    case Decl::Kind::kBits:
    case Decl::Kind::kEnum:
      return types::Resourceness::kValue;
    case Decl::Kind::kProtocol:
      return types::Resourceness::kResource;
    case Decl::Kind::kStruct:
      ZX_ASSERT_MSG(decl->compiled, "accessing resourceness of not-yet-compiled struct");
      return static_cast<const Struct*>(decl)->resourceness.value();
    case Decl::Kind::kTable:
      return static_cast<const Table*>(decl)->resourceness;
    case Decl::Kind::kUnion:
      ZX_ASSERT_MSG(decl->compiled, "accessing resourceness of not-yet-compiled union");
      return static_cast<const Union*>(decl)->resourceness.value();
    case Decl::Kind::kOverlay:
      ZX_ASSERT_MSG(decl->compiled, "accessing resourceness of not-yet-compiled overlay");
      return static_cast<const Overlay*>(decl)->resourceness;
    case Decl::Kind::kNewType: {
      const auto* new_type = static_cast<const NewType*>(decl);
      const auto* underlying_type = new_type->type_ctor->type;
      ZX_ASSERT_MSG(underlying_type, "accessing resourceness of not-yet-compiled new-type");
      return underlying_type->Resourceness();
    }
    case Decl::Kind::kBuiltin:
    case Decl::Kind::kConst:
    case Decl::Kind::kResource:
    case Decl::Kind::kService:
    case Decl::Kind::kAlias:
      ZX_PANIC("unexpected kind");
  }
}

std::any ArrayType::AcceptAny(VisitorAny* visitor) const { return visitor->Visit(*this); }
std::any BoxType::AcceptAny(VisitorAny* visitor) const { return visitor->Visit(*this); }
std::any HandleType::AcceptAny(VisitorAny* visitor) const { return visitor->Visit(*this); }
std::any IdentifierType::AcceptAny(VisitorAny* visitor) const { return visitor->Visit(*this); }
std::any PrimitiveType::AcceptAny(VisitorAny* visitor) const { return visitor->Visit(*this); }
std::any InternalType::AcceptAny(VisitorAny* visitor) const { return visitor->Visit(*this); }
std::any StringType::AcceptAny(VisitorAny* visitor) const { return visitor->Visit(*this); }
std::any TransportSideType::AcceptAny(VisitorAny* visitor) const { return visitor->Visit(*this); }
std::any VectorType::AcceptAny(VisitorAny* visitor) const { return visitor->Visit(*this); }
std::any ZxExperimentalPointerType::AcceptAny(VisitorAny* visitor) const {
  return visitor->Visit(*this);
}

std::any UntypedNumericType::AcceptAny(VisitorAny* visitor) const {
  ZX_PANIC("should not have untyped numeric here");
}

}  // namespace fidl::flat
