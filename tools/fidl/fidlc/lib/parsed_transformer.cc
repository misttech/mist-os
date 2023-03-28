// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fit/function.h>
#include <zircon/assert.h>

#include "tools/fidl/fidlc/include/fidl/transformer.h"
#include "tools/fidl/fidlc/include/fidl/tree_visitor.h"

namespace fidl::fix {

bool ParsedTransformer::Prepare() {
  ZX_ASSERT(step() == Step::kNew);
  NextStep();
  BuildTransformStates();

  if (HasErrors()) {
    return false;
  }
  NextStep();
  return true;
}

void ParsedTransformer::OnAliasDeclaration(const std::unique_ptr<raw::AliasDeclaration>& el) {
  VISIT_THEN_TRANSFORM_UNIQUE_PTR(AliasDeclaration, AliasDeclaration);
}

void ParsedTransformer::OnAttributeArg(const std::unique_ptr<raw::AttributeArg>& el) {
  VISIT_THEN_TRANSFORM_UNIQUE_PTR(AttributeArg, AttributeArg);
}

void ParsedTransformer::OnAttribute(const std::unique_ptr<raw::Attribute>& el) {
  VISIT_THEN_TRANSFORM_UNIQUE_PTR(Attribute, Attribute);
}

void ParsedTransformer::OnBinaryOperatorConstant(
    const std::unique_ptr<raw::BinaryOperatorConstant>& el) {
  VISIT_THEN_TRANSFORM_UNIQUE_PTR(BinaryOperatorConstant, BinaryOperatorConstant);
}

void ParsedTransformer::OnBoolLiteral(raw::BoolLiteral& el) {
  VISIT_THEN_TRANSFORM_REFERENCE(BoolLiteral);
}

void ParsedTransformer::OnConstDeclaration(const std::unique_ptr<raw::ConstDeclaration>& el) {
  VISIT_THEN_TRANSFORM_UNIQUE_PTR(ConstDeclaration, ConstDeclaration);
}

void ParsedTransformer::OnDocCommentLiteral(raw::DocCommentLiteral& el) {
  VISIT_THEN_TRANSFORM_REFERENCE(DocCommentLiteral);
}

void ParsedTransformer::OnIdentifierConstant(const std::unique_ptr<raw::IdentifierConstant>& el) {
  VISIT_THEN_TRANSFORM_UNIQUE_PTR(IdentifierConstant, IdentifierConstant);
}

void ParsedTransformer::OnIdentifierLayoutParameter(
    const std::unique_ptr<raw::IdentifierLayoutParameter>& el) {
  VISIT_THEN_TRANSFORM_UNIQUE_PTR(IdentifierLayoutParameter, IdentifierLayoutParameter);
}

void ParsedTransformer::OnLayout(const std::unique_ptr<raw::Layout>& el) {
  switch (el->kind) {
    case raw::Layout::kBits: {
      VISIT_THEN_TRANSFORM_UNIQUE_PTR(Layout, BitsDeclaration);
      break;
    }
    case raw::Layout::kEnum: {
      VISIT_THEN_TRANSFORM_UNIQUE_PTR(Layout, EnumDeclaration);
      break;
    }
    case raw::Layout::kStruct: {
      VISIT_THEN_TRANSFORM_UNIQUE_PTR(Layout, StructDeclaration);
      break;
    }
    case raw::Layout::kTable: {
      VISIT_THEN_TRANSFORM_UNIQUE_PTR(Layout, TableDeclaration);
      break;
    }
    case raw::Layout::kUnion: {
      VISIT_THEN_TRANSFORM_UNIQUE_PTR(Layout, UnionDeclaration);
      break;
    }
    case raw::Layout::kOverlay: {
      VISIT_THEN_TRANSFORM_UNIQUE_PTR(Layout, OverlayDeclaration);
      break;
    }
  }
}

void ParsedTransformer::OnLayoutParameterList(const std::unique_ptr<raw::LayoutParameterList>& el) {
  VISIT_THEN_TRANSFORM_UNIQUE_PTR(LayoutParameterList, LayoutParameterList);
}

void ParsedTransformer::OnLiteralConstant(const std::unique_ptr<raw::LiteralConstant>& el) {
  VISIT_THEN_TRANSFORM_UNIQUE_PTR(LiteralConstant, LiteralConstant);
}

void ParsedTransformer::OnLiteralLayoutParameter(
    const std::unique_ptr<raw::LiteralLayoutParameter>& el) {
  VISIT_THEN_TRANSFORM_UNIQUE_PTR(LiteralLayoutParameter, LiteralLayoutParameter);
  static_cast<void>(uptr.release());
}

void ParsedTransformer::OnNumericLiteral(raw::NumericLiteral& el) {
  VISIT_THEN_TRANSFORM_REFERENCE(NumericLiteral);
}

void ParsedTransformer::OnOrdinaledLayoutMember(
    const std::unique_ptr<raw::OrdinaledLayoutMember>& el) {
  switch (el->layout_kind) {
    case raw::Layout::kTable: {
      VISIT_THEN_TRANSFORM_UNIQUE_PTR(OrdinaledLayoutMember, TableMember);
      break;
    }
    case raw::Layout::kUnion: {
      VISIT_THEN_TRANSFORM_UNIQUE_PTR(OrdinaledLayoutMember, UnionMember);
      break;
    }
    default:
      ZX_ASSERT_MSG(false, "should be unreachable");
  }
}

void ParsedTransformer::OnProtocolCompose(const std::unique_ptr<raw::ProtocolCompose>& el) {
  VISIT_THEN_TRANSFORM_UNIQUE_PTR(ProtocolCompose, ProtocolCompose);
}

void ParsedTransformer::OnProtocolDeclaration(const std::unique_ptr<raw::ProtocolDeclaration>& el) {
  VISIT_THEN_TRANSFORM_UNIQUE_PTR(ProtocolDeclaration, ProtocolDeclaration);
}

void ParsedTransformer::OnProtocolMethod(const std::unique_ptr<raw::ProtocolMethod>& el) {
  VISIT_THEN_TRANSFORM_UNIQUE_PTR(ProtocolMethod, ProtocolMethod);
}

void ParsedTransformer::OnResourceDeclaration(const std::unique_ptr<raw::ResourceDeclaration>& el) {
  VISIT_THEN_TRANSFORM_UNIQUE_PTR(ResourceDeclaration, ResourceDeclaration);
}

void ParsedTransformer::OnResourceProperty(const std::unique_ptr<raw::ResourceProperty>& el) {
  VISIT_THEN_TRANSFORM_UNIQUE_PTR(ResourceProperty, ResourceProperty);
}

void ParsedTransformer::OnServiceDeclaration(const std::unique_ptr<raw::ServiceDeclaration>& el) {
  VISIT_THEN_TRANSFORM_UNIQUE_PTR(ServiceDeclaration, ServiceDeclaration);
}

void ParsedTransformer::OnServiceMember(const std::unique_ptr<raw::ServiceMember>& el) {
  VISIT_THEN_TRANSFORM_UNIQUE_PTR(ServiceMember, ServiceMember);
}

void ParsedTransformer::OnStringLiteral(raw::StringLiteral& el) {
  VISIT_THEN_TRANSFORM_REFERENCE(StringLiteral);
}

void ParsedTransformer::OnStructLayoutMember(const std::unique_ptr<raw::StructLayoutMember>& el) {
  VISIT_THEN_TRANSFORM_UNIQUE_PTR(StructLayoutMember, StructMember);
}

void ParsedTransformer::OnTypeConstraints(const std::unique_ptr<raw::TypeConstraints>& el) {
  VISIT_THEN_TRANSFORM_UNIQUE_PTR(TypeConstraints, TypeConstraints);
}

void ParsedTransformer::OnTypeConstructor(const std::unique_ptr<raw::TypeConstructor>& el) {
  VISIT_THEN_TRANSFORM_UNIQUE_PTR(TypeConstructor, TypeConstructor);
}

void ParsedTransformer::OnTypeLayoutParameter(const std::unique_ptr<raw::TypeLayoutParameter>& el) {
  VISIT_THEN_TRANSFORM_UNIQUE_PTR(TypeLayoutParameter, TypeLayoutParameter);
}

void ParsedTransformer::OnValueLayoutMember(const std::unique_ptr<raw::ValueLayoutMember>& el) {
  switch (el->layout_kind) {
    case raw::Layout::kBits: {
      VISIT_THEN_TRANSFORM_UNIQUE_PTR(ValueLayoutMember, BitsMember);
      break;
    }
    case raw::Layout::kEnum: {
      VISIT_THEN_TRANSFORM_UNIQUE_PTR(ValueLayoutMember, EnumMember);
      break;
    }
    default:
      ZX_ASSERT_MSG(false, "should be unreachable");
  }
}

}  // namespace fidl::fix
