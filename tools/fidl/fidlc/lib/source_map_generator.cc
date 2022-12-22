// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "tools/fidl/fidlc/include/fidl/source_map_generator.h"

#include <zircon/assert.h>

#include "tools/fidl/fidlc/include/fidl/flat/name.h"
#include "tools/fidl/fidlc/include/fidl/flat/types.h"
#include "tools/fidl/fidlc/include/fidl/flat_ast.h"
#include "tools/fidl/fidlc/include/fidl/names.h"
#include "tools/fidl/fidlc/include/fidl/source_map.h"
#include "tools/fidl/fidlc/include/fidl/types.h"

namespace fidl {

void SourceMapGenerator::Generate(const flat::Alias* value) {
  if (value == nullptr) {
    return;
  }
  builder_.AddVersioned<flat::Alias>(value);
  Generate(value->partial_type_ctor.get());
}

void SourceMapGenerator::Generate(const flat::Attribute* value) {
  if (value != nullptr && value->maybe_source_signature().has_value()) {
    builder_.AddUnique<flat::Attribute>(value);
    for (const auto& arg : value->args) {
      Generate(arg.get());
    }
  }
}

void SourceMapGenerator::Generate(const flat::AttributeArg* value) {
  if (value == nullptr) {
    return;
  }
  builder_.AddUnique<flat::AttributeArg>(value);
  Generate(value->value.get());
}

void SourceMapGenerator::Generate(const flat::AttributeList* value) {
  if (value == nullptr) {
    return;
  }
  for (const auto& attribute : value->attributes) {
    Generate(attribute.get());
  }
}

void SourceMapGenerator::Generate(const flat::BinaryOperatorConstant* value) {
  if (value == nullptr) {
    return;
  }
  builder_.AddUnique<flat::BinaryOperatorConstant>(value);
  Generate(value->left_operand.get());
  Generate(value->right_operand.get());
}

void SourceMapGenerator::Generate(const flat::Bits* value) {
  if (value == nullptr) {
    return;
  }
  builder_.AddVersioned<flat::Bits>(value);
  Generate(value->attributes.get());
  Generate(value->subtype_ctor.get());
  for (const auto& member : value->members) {
    Generate(&member);
  }
}

void SourceMapGenerator::Generate(const flat::Bits::Member* value) {
  if (value == nullptr) {
    return;
  }
  builder_.AddVersioned<flat::Bits::Member>(value);
  Generate(value->attributes.get());
  Generate(value->value.get());
}

void SourceMapGenerator::Generate(const flat::Const* value) {
  if (value == nullptr) {
    return;
  }
  builder_.AddVersioned<flat::Const>(value);
  Generate(value->attributes.get());
  Generate(value->type_ctor.get());
  Generate(value->value.get());
}

void SourceMapGenerator::Generate(const flat::Constant* value) {
  if (value == nullptr) {
    return;
  }
  switch (value->kind) {
    case flat::Constant::Kind::kBinaryOperator: {
      auto binary_op = static_cast<const flat::BinaryOperatorConstant*>(value);
      Generate(binary_op);
      break;
    }
    case flat::Constant::Kind::kIdentifier: {
      auto identifier = static_cast<const flat::IdentifierConstant*>(value);
      Generate(identifier);
      break;
    }
    case flat::Constant::Kind::kLiteral: {
      auto literal = static_cast<const flat::LiteralConstant*>(value);
      Generate(literal);
      break;
    }
  }
}

void SourceMapGenerator::Generate(const flat::Enum* value) {
  if (value == nullptr) {
    return;
  }
  builder_.AddVersioned<flat::Enum>(value);
  Generate(value->attributes.get());
  Generate(value->subtype_ctor.get());
  for (const auto& member : value->members) {
    Generate(&member);
  }
}

void SourceMapGenerator::Generate(const flat::Enum::Member* value) {
  if (value == nullptr) {
    return;
  }
  builder_.AddVersioned<flat::Enum::Member>(value);
  Generate(value->attributes.get());
  Generate(value->value.get());
}

void SourceMapGenerator::Generate(const flat::IdentifierConstant* value) {
  if (value == nullptr) {
    return;
  }
  builder_.AddUnique<flat::IdentifierConstant>(value);
}

void SourceMapGenerator::Generate(const flat::IdentifierLayoutParameter* value) {
  if (value == nullptr) {
    return;
  }
  builder_.AddUnique<flat::IdentifierLayoutParameter>(value);
  Generate(value->as_type_ctor.get());
  Generate(value->as_constant.get());
}

void SourceMapGenerator::Generate(const flat::LayoutParameter* value) {
  if (value == nullptr) {
    return;
  }
  switch (value->kind) {
    case flat::LayoutParameter::Kind::kIdentifier: {
      auto identifier = static_cast<const flat::IdentifierLayoutParameter*>(value);
      Generate(identifier);
      break;
    }
    case flat::LayoutParameter::Kind::kLiteral: {
      auto literal = static_cast<const flat::LiteralLayoutParameter*>(value);
      Generate(literal);
      break;
    }
    case flat::LayoutParameter::Kind::kType: {
      auto typ = static_cast<const flat::TypeLayoutParameter*>(value);
      Generate(typ);
      break;
    }
  }
}

void SourceMapGenerator::Generate(const flat::LayoutParameterList* value) {
  if (value != nullptr && value->maybe_source_signature().has_value()) {
    builder_.AddUnique<flat::LayoutParameterList>(value);
    for (const auto& item : value->items) {
      Generate(item.get());
    }
  }
}

void SourceMapGenerator::Generate(const flat::LiteralConstant* value) {
  if (value == nullptr) {
    return;
  }
  builder_.AddUnique<flat::LiteralConstant>(value);
}

void SourceMapGenerator::Generate(const flat::LiteralLayoutParameter* value) {
  if (value == nullptr) {
    return;
  }
  builder_.AddUnique<flat::LiteralLayoutParameter>(value);
  Generate(value->literal.get());
}

void SourceMapGenerator::Generate(const flat::Protocol* value) {
  if (value == nullptr) {
    return;
  }
  builder_.AddVersioned<flat::Protocol>(value);
  Generate(value->attributes.get());
  for (const auto& composed : value->composed_protocols) {
    Generate(&composed);
  }
  for (const auto& method : value->methods) {
    Generate(&method);
  }
}

void SourceMapGenerator::Generate(const flat::Protocol::ComposedProtocol* value) {
  if (value == nullptr) {
    return;
  }
  builder_.AddVersioned<flat::Protocol::ComposedProtocol>(value);
  Generate(value->attributes.get());
}

void SourceMapGenerator::Generate(const flat::Protocol::Method* value) {
  if (value == nullptr) {
    return;
  }
  builder_.AddVersioned<flat::Protocol::Method>(value);
  Generate(value->attributes.get());
  Generate(value->maybe_request.get());
  Generate(value->maybe_response.get());
}

void SourceMapGenerator::Generate(const flat::Resource* value) {
  if (value == nullptr) {
    return;
  }
  builder_.AddVersioned<flat::Resource>(value);
  Generate(value->attributes.get());
  Generate(value->subtype_ctor.get());
  for (const auto& property : value->properties) {
    Generate(&property);
  }
}

void SourceMapGenerator::Generate(const flat::Resource::Property* value) {
  if (value == nullptr) {
    return;
  }
  builder_.AddVersioned<flat::Resource::Property>(value);
  Generate(value->attributes.get());
  Generate(value->type_ctor.get());
}

void SourceMapGenerator::Generate(const flat::Service* value) {
  if (value == nullptr) {
    return;
  }
  builder_.AddVersioned<flat::Service>(value);
  Generate(value->attributes.get());
  for (const auto& member : value->members) {
    Generate(&member);
  }
}

void SourceMapGenerator::Generate(const flat::Service::Member* value) {
  if (value == nullptr) {
    return;
  }
  builder_.AddVersioned<flat::Service::Member>(value);
  Generate(value->attributes.get());
  Generate(value->type_ctor.get());
}

void SourceMapGenerator::Generate(const flat::Struct* value) {
  if (value == nullptr) {
    return;
  }
  builder_.AddVersioned<flat::Struct>(value);
  Generate(value->attributes.get());
  for (const auto& member : value->members) {
    Generate(&member);
  }
}

void SourceMapGenerator::Generate(const flat::Struct::Member* value) {
  if (value == nullptr) {
    return;
  }
  builder_.AddVersioned<flat::Struct::Member>(value);
  Generate(value->attributes.get());
  Generate(value->type_ctor.get());
  Generate(value->maybe_default_value.get());
}

void SourceMapGenerator::Generate(const flat::Table* value) {
  if (value == nullptr) {
    return;
  }
  builder_.AddVersioned<flat::Table>(value);
  Generate(value->attributes.get());
  for (const auto& member : value->members) {
    Generate(&member);
  }
}

void SourceMapGenerator::Generate(const flat::Table::Member* value) {
  if (value == nullptr) {
    return;
  }
  builder_.AddVersioned<flat::Table::Member>(value);
  Generate(value->attributes.get());
  if (value->maybe_used) {
    Generate(value->maybe_used->type_ctor.get());
  }
}

void SourceMapGenerator::Generate(const flat::TypeConstraints* value) {
  if (value != nullptr && value->maybe_source_signature().has_value()) {
    builder_.AddUnique<flat::TypeConstraints>(value);
    for (const auto& item : value->items) {
      Generate(item.get());
    }
  }
}

void SourceMapGenerator::Generate(const flat::TypeConstructor* value) {
  if (value != nullptr && value->maybe_source_signature().has_value()) {
    builder_.AddUnique<flat::TypeConstructor>(value);
    Generate(value->parameters.get());
    Generate(value->constraints.get());
  }
}

void SourceMapGenerator::Generate(const flat::TypeLayoutParameter* value) {
  if (value == nullptr) {
    return;
  }
  builder_.AddUnique<flat::TypeLayoutParameter>(value);
  Generate(value->type_ctor.get());
}

void SourceMapGenerator::Generate(const flat::Union* value) {
  if (value == nullptr) {
    return;
  }
  builder_.AddVersioned<flat::Union>(value);
  Generate(value->attributes.get());
  for (const auto& member : value->members) {
    Generate(&member);
  }
}

void SourceMapGenerator::Generate(const flat::Union::Member* value) {
  if (value == nullptr) {
    return;
  }
  builder_.AddVersioned<flat::Union::Member>(value);
  Generate(value->attributes.get());
  if (value->maybe_used) {
    Generate(value->maybe_used->type_ctor.get());
  }
}

SourceMap SourceMapGenerator::Produce() {
  Generate(library_->attributes.get());

  for (const auto& decl : library_->declarations.aliases) {
    Generate(decl.get());
  }
  for (const auto& decl : library_->declarations.bits) {
    Generate(decl.get());
  }
  for (const auto& decl : library_->declarations.consts) {
    Generate(decl.get());
  }
  for (const auto& decl : library_->declarations.enums) {
    Generate(decl.get());
  }
  for (const auto& decl : library_->declarations.protocols) {
    Generate(decl.get());
  }
  for (const auto& decl : library_->declarations.resources) {
    Generate(decl.get());
  }
  for (const auto& decl : library_->declarations.services) {
    Generate(decl.get());
  }
  for (const auto& decl : library_->declarations.structs) {
    Generate(decl.get());
  }
  for (const auto& decl : library_->declarations.tables) {
    Generate(decl.get());
  }
  for (const auto& decl : library_->declarations.unions) {
    Generate(decl.get());
  }

  return std::move(builder_).Build();
}

}  // namespace fidl
