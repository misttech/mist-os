// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef TOOLS_FIDL_FIDLC_INCLUDE_FIDL_SOURCE_MAP_GENERATOR_H_
#define TOOLS_FIDL_FIDLC_INCLUDE_FIDL_SOURCE_MAP_GENERATOR_H_

#include <zircon/assert.h>

#include "tools/fidl/fidlc/include/fidl/experimental_flags.h"
#include "tools/fidl/fidlc/include/fidl/flat/compiler.h"
#include "tools/fidl/fidlc/include/fidl/flat_ast.h"
#include "tools/fidl/fidlc/include/fidl/source_map.h"

namespace fidl {

// Generate a |SourceMap| for a given flat AST. We build the map by using |.get()| calls into the
// passed in |flat::Library|'s unique pointers, meaning that the |flat::Library| must outlive the
// generated |SourceMap|, and must not be mutated after this generation has occurred.
class SourceMapGenerator {
 public:
  explicit SourceMapGenerator(const flat::Library* library, ExperimentalFlags experimental_flags)
      : library_(library), experimental_flags_(experimental_flags) {}

  ~SourceMapGenerator() = default;

  SourceMap Produce();

  void Generate(const flat::Alias* value);
  void Generate(const flat::Attribute* value);
  void Generate(const flat::AttributeArg* value);
  void Generate(const flat::AttributeList* value);
  void Generate(const flat::BinaryOperatorConstant* value);
  void Generate(const flat::Bits* value);
  void Generate(const flat::Bits::Member* value);
  void Generate(const flat::Const* value);
  void Generate(const flat::Constant* value);
  void Generate(const flat::Enum* value);
  void Generate(const flat::Enum::Member* value);
  void Generate(const flat::IdentifierConstant* value);
  void Generate(const flat::IdentifierLayoutParameter* value);
  void Generate(const flat::LayoutParameter* value);
  void Generate(const flat::LayoutParameterList* value);
  void Generate(const flat::LiteralConstant* value);
  void Generate(const flat::LiteralLayoutParameter* value);
  void Generate(const flat::Protocol* value);
  void Generate(const flat::Protocol::ComposedProtocol* value);
  void Generate(const flat::Protocol::Method* value);
  void Generate(const flat::Resource* value);
  void Generate(const flat::Resource::Property* value);
  void Generate(const flat::Service* value);
  void Generate(const flat::Service::Member* value);
  void Generate(const flat::Struct* value);
  void Generate(const flat::Struct::Member* value);
  void Generate(const flat::Table* value);
  void Generate(const flat::Table::Member* value);
  void Generate(const flat::TypeConstraints* value);
  void Generate(const flat::TypeConstructor* value);
  void Generate(const flat::TypeLayoutParameter* value);
  void Generate(const flat::Union* value);
  void Generate(const flat::Union::Member* value);

 private:
  const flat::Library* library_;
  const ExperimentalFlags experimental_flags_;
  SourceMapBuilder builder_;
};

}  // namespace fidl

#endif  // TOOLS_FIDL_FIDLC_INCLUDE_FIDL_SOURCE_MAP_GENERATOR_H_
