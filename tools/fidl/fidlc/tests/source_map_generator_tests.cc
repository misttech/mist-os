// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zxtest/zxtest.h>

#include "tools/fidl/fidlc/include/fidl/source_map_generator.h"
#include "tools/fidl/fidlc/include/fidl/versioning_types.h"
#include "tools/fidl/fidlc/tests/error_test.h"
#include "tools/fidl/fidlc/tests/test_library.h"

namespace {

TEST(SourceMapGeneratorTests, GoodEmptyLibrary) {
  TestLibrary library(R"FIDL(library example;
)FIDL");
  ASSERT_COMPILED(library);

  auto source_map = library.GenerateSourceMap("example");
  EXPECT_TRUE(source_map.empty());
}

TEST(SourceMapGeneratorTests, GoodAlias) {
  TestLibrary library(R"FIDL(library example;

alias Unversioned = vector<uint8>:100;

)FIDL");
  ASSERT_COMPILED(library);
  std::unique_ptr<fidl::raw::File> ast;
  ASSERT_TRUE(library.Parse(&ast));

  auto source_map = library.GenerateSourceMap("example");
  EXPECT_EQ(source_map.size(), 6);

  auto raw_alias = std::move(ast->alias_list[0]);
  auto alias_versions = source_map.GetVersioned<fidl::flat::Alias>(raw_alias->source_signature());
  EXPECT_EQ(alias_versions->size(), 1);
  EXPECT_EQ(alias_versions->Oldest(), alias_versions->Newest());
  EXPECT_EQ(alias_versions->Oldest(), alias_versions->At(fidl::Version::Head()));
  EXPECT_NULL(alias_versions->At(fidl::Version::From(2).value()));
  EXPECT_SUBSTR(alias_versions->Oldest()->GetName(), "Unversioned");

  auto raw_type_ctor = std::move(raw_alias->type_ctor);
  auto type_ctor_unique =
      source_map.GetUnique<fidl::flat::TypeConstructor>(raw_type_ctor->source_signature());
  EXPECT_NOT_NULL(type_ctor_unique);
  EXPECT_STREQ(type_ctor_unique->Get()->type->name.decl_name().data(), "vector");

  auto raw_layout_params = std::move(raw_type_ctor->parameters);
  auto layout_params_unique =
      source_map.GetUnique<fidl::flat::LayoutParameterList>(raw_layout_params->source_signature());
  EXPECT_NOT_NULL(layout_params_unique);
  EXPECT_EQ(layout_params_unique->Get()->items.size(), 1);

  auto raw_layout_param = std::move(raw_layout_params->items[0]);
  auto layout_param_unique =
      source_map.GetUnique<fidl::flat::LayoutParameter>(raw_layout_param->source_signature());
  EXPECT_NOT_NULL(layout_param_unique);
  EXPECT_STREQ(type_ctor_unique->Get()->type->name.decl_name().data(), "vector");

  auto raw_type_constraints = std::move(raw_type_ctor->constraints);
  auto type_constraints_unique =
      source_map.GetUnique<fidl::flat::TypeConstraints>(raw_type_constraints->source_signature());
  EXPECT_NOT_NULL(type_constraints_unique);
  EXPECT_EQ(type_constraints_unique->Get()->items.size(), 1);

  auto raw_type_constraint = std::move(raw_type_constraints->items[0]);
  auto type_constraint_unique =
      source_map.GetUnique<fidl::flat::Constant>(raw_type_constraint->source_signature());
  EXPECT_NOT_NULL(type_constraint_unique);
  EXPECT_EQ(static_cast<const fidl::flat::NumericConstantValue<uint32_t>*>(
                &type_constraint_unique->Get()->Value())
                ->value,
            100);
}

TEST(SourceMapGeneratorTests, GoodAttribute) {
  TestLibrary library(R"FIDL(
@foo(bar=true)
library example;
)FIDL");
  ASSERT_COMPILED(library);
  std::unique_ptr<fidl::raw::File> ast;
  ASSERT_TRUE(library.Parse(&ast));

  auto source_map = library.GenerateSourceMap("example");
  EXPECT_EQ(source_map.size(), 3);

  auto raw_attr = std::move(ast->library_decl->attributes->attributes[0]);
  auto attr_unique = source_map.GetUnique<fidl::flat::Attribute>(raw_attr->source_signature());
  EXPECT_NOT_NULL(attr_unique);
  EXPECT_SUBSTR(attr_unique->Get()->name.data(), "foo");

  auto raw_attr_arg = std::move(raw_attr->args[0]);
  auto attr_arg_unique =
      source_map.GetUnique<fidl::flat::AttributeArg>(raw_attr_arg->source_signature());
  EXPECT_NOT_NULL(attr_arg_unique);
  EXPECT_SUBSTR(attr_arg_unique->Get()->name->data(), "bar");

  auto raw_constant = std::move(raw_attr_arg->value);
  auto constant_unique =
      source_map.GetUnique<fidl::flat::Constant>(raw_constant->source_signature());
  EXPECT_NOT_NULL(constant_unique);
  EXPECT_EQ(
      static_cast<const fidl::flat::BoolConstantValue*>(&constant_unique->Get()->Value())->value,
      true);
}

TEST(SourceMapGeneratorTests, GoodBitsUnversioned) {
  TestLibrary library(R"FIDL(library example;

type Unversioned = bits : uint32 {
  MEMBER = 1;
};

)FIDL");
  ASSERT_COMPILED(library);
  std::unique_ptr<fidl::raw::File> ast;
  ASSERT_TRUE(library.Parse(&ast));

  auto source_map = library.GenerateSourceMap("example");
  EXPECT_EQ(source_map.size(), 4);

  auto raw_type_decl = std::move(ast->type_decls[0]);
  auto raw_inline_ref =
      static_cast<fidl::raw::InlineLayoutReference*>(raw_type_decl->type_ctor->layout_ref.get());
  auto raw_layout = std::move(raw_inline_ref->layout);
  auto bits_versions = source_map.GetVersioned<fidl::flat::Bits>(raw_layout->source_signature());
  EXPECT_EQ(bits_versions->size(), 1);
  EXPECT_EQ(bits_versions->Oldest(), bits_versions->Newest());
  EXPECT_EQ(bits_versions->Oldest(), bits_versions->At(fidl::Version::Head()));
  EXPECT_NULL(bits_versions->At(fidl::Version::From(2).value()));
  EXPECT_SUBSTR(bits_versions->Oldest()->GetName(), "Unversioned");

  auto raw_subtype_ctor = std::move(raw_layout->subtype_ctor);
  auto subtype_ctor_unique =
      source_map.GetUnique<fidl::flat::TypeConstructor>(raw_subtype_ctor->source_signature());
  EXPECT_NOT_NULL(subtype_ctor_unique);
  EXPECT_EQ(subtype_ctor_unique->Get()->type->kind, fidl::flat::Type::Kind::kPrimitive);

  auto raw_member = std::move(raw_layout->members[0]);
  auto raw_bits_member = static_cast<fidl::raw::ValueLayoutMember*>(raw_member.get());
  auto bits_member_versions =
      source_map.GetVersioned<fidl::flat::Bits::Member>(raw_bits_member->source_signature());
  EXPECT_EQ(bits_member_versions->size(), 1);
  EXPECT_EQ(bits_member_versions->Oldest(), bits_member_versions->Newest());
  EXPECT_EQ(bits_member_versions->Oldest(), bits_member_versions->At(fidl::Version::Head()));
  EXPECT_EQ(bits_member_versions->Oldest()->name.data(), "MEMBER");

  auto raw_constant = std::move(raw_bits_member->value);
  auto constant_unique =
      source_map.GetUnique<fidl::flat::Constant>(raw_constant->source_signature());
  EXPECT_NOT_NULL(constant_unique);
  EXPECT_EQ(static_cast<const fidl::flat::NumericConstantValue<uint32_t>*>(
                &constant_unique->Get()->Value())
                ->value,
            1);
}

TEST(SourceMapGeneratorTests, GoodBitsVersioned) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Versioned = flexible bits : uint32 {
  @available(removed=2)
  OLD = 1;
  @available(added=2)
  NEW = 2;
};

)FIDL");
  library.SelectVersion("example", "HEAD");
  ASSERT_COMPILED(library);
  std::unique_ptr<fidl::raw::File> ast;
  ASSERT_TRUE(library.Parse(&ast));

  auto source_map = library.GenerateSourceMap("example");

  auto raw_type_decl = std::move(ast->type_decls[0]);
  auto raw_inline_ref =
      static_cast<fidl::raw::InlineLayoutReference*>(raw_type_decl->type_ctor->layout_ref.get());
  auto raw_layout = std::move(raw_inline_ref->layout);
  auto bits_versions = source_map.GetVersioned<fidl::flat::Bits>(raw_layout->source_signature());
  EXPECT_EQ(bits_versions->size(), 2);
  EXPECT_NE(bits_versions->Oldest(), bits_versions->Newest());
  EXPECT_EQ(bits_versions->Oldest(), bits_versions->At(fidl::Version::From(1).value()));
  EXPECT_EQ(bits_versions->Newest(), bits_versions->At(fidl::Version::From(2).value()));
  EXPECT_SUBSTR(bits_versions->Oldest()->GetName(), "Versioned");

  auto raw_subtype_ctor = std::move(raw_layout->subtype_ctor);
  auto subtype_ctor_unique =
      source_map.GetUnique<fidl::flat::TypeConstructor>(raw_subtype_ctor->source_signature());
  EXPECT_NOT_NULL(subtype_ctor_unique);
  EXPECT_EQ(subtype_ctor_unique->Get()->type->kind, fidl::flat::Type::Kind::kPrimitive);

  auto raw_removed_member = std::move(raw_layout->members[0]);
  auto raw_removed_bits_member =
      static_cast<fidl::raw::ValueLayoutMember*>(raw_removed_member.get());
  auto bits_removed_member_versions = source_map.GetVersioned<fidl::flat::Bits::Member>(
      raw_removed_bits_member->source_signature());
  EXPECT_EQ(bits_removed_member_versions->size(), 1);
  EXPECT_EQ(bits_removed_member_versions->Oldest(), bits_removed_member_versions->Newest());
  EXPECT_EQ(bits_removed_member_versions->Oldest(),
            bits_removed_member_versions->At(fidl::Version::From(1).value()));
  EXPECT_EQ(bits_removed_member_versions->Oldest()->name.data(), "OLD");

  auto raw_removed_constant = std::move(raw_removed_bits_member->value);
  auto removed_constant_unique =
      source_map.GetUnique<fidl::flat::Constant>(raw_removed_constant->source_signature());
  EXPECT_NOT_NULL(removed_constant_unique);
  EXPECT_NOT_NULL(removed_constant_unique);
  EXPECT_EQ(static_cast<const fidl::flat::NumericConstantValue<uint32_t>*>(
                &removed_constant_unique->Get()->Value())
                ->value,
            1);

  auto raw_added_member = std::move(raw_layout->members[1]);
  auto raw_added_bits_member = static_cast<fidl::raw::ValueLayoutMember*>(raw_added_member.get());
  auto bits_added_member_versions =
      source_map.GetVersioned<fidl::flat::Bits::Member>(raw_added_bits_member->source_signature());
  EXPECT_EQ(bits_added_member_versions->size(), 1);
  EXPECT_EQ(bits_added_member_versions->Oldest(), bits_added_member_versions->Newest());
  EXPECT_EQ(bits_added_member_versions->Oldest(), bits_added_member_versions->Newest());
  EXPECT_EQ(bits_added_member_versions->Oldest(),
            bits_added_member_versions->At(fidl::Version::From(2).value()));
  EXPECT_EQ(bits_added_member_versions->Oldest()->name.data(), "NEW");

  auto raw_added_constant = std::move(raw_added_bits_member->value);
  auto added_constant_unique =
      source_map.GetUnique<fidl::flat::Constant>(raw_added_constant->source_signature());
  EXPECT_NOT_NULL(added_constant_unique);
  EXPECT_NOT_NULL(added_constant_unique);
  EXPECT_EQ(static_cast<const fidl::flat::NumericConstantValue<uint32_t>*>(
                &added_constant_unique->Get()->Value())
                ->value,
            2);
}

TEST(SourceMapGeneratorTests, GoodConst) {
  TestLibrary library(R"FIDL(library example;

const LITERAL uint8 = 1;
const BIN_OP uint8 = 2 | LITERAL;

)FIDL");
  ASSERT_COMPILED(library);
  std::unique_ptr<fidl::raw::File> ast;
  ASSERT_TRUE(library.Parse(&ast));

  auto source_map = library.GenerateSourceMap("example");
  EXPECT_EQ(source_map.size(), 8);

  auto raw_literal_const = std::move(ast->const_declaration_list[0]);
  auto literal_const_versions =
      source_map.GetVersioned<fidl::flat::Const>(raw_literal_const->source_signature());
  EXPECT_EQ(literal_const_versions->size(), 1);
  EXPECT_EQ(literal_const_versions->Oldest(), literal_const_versions->Newest());
  EXPECT_EQ(literal_const_versions->Oldest(), literal_const_versions->At(fidl::Version::Head()));
  EXPECT_SUBSTR(literal_const_versions->Oldest()->GetName(), "LITERAL");

  auto raw_literal_type_ctor = std::move(raw_literal_const->type_ctor);
  auto literal_type_ctor_unique =
      source_map.GetUnique<fidl::flat::TypeConstructor>(raw_literal_type_ctor->source_signature());
  EXPECT_NOT_NULL(literal_type_ctor_unique);
  EXPECT_STREQ(literal_type_ctor_unique->Get()->type->name.decl_name().data(), "uint8");

  auto raw_literal_constant = std::move(raw_literal_const->constant);
  auto literal_constant_unique =
      source_map.GetUnique<fidl::flat::Constant>(raw_literal_constant->source_signature());
  EXPECT_NOT_NULL(literal_constant_unique);
  EXPECT_NOT_NULL(literal_constant_unique);
  EXPECT_EQ(static_cast<const fidl::flat::NumericConstantValue<uint8_t>*>(
                &literal_constant_unique->Get()->Value())
                ->value,
            1);

  auto raw_bin_op_const = std::move(ast->const_declaration_list[1]);
  auto bin_op_const_versions =
      source_map.GetVersioned<fidl::flat::Const>(raw_bin_op_const->source_signature());
  EXPECT_EQ(bin_op_const_versions->size(), 1);
  EXPECT_EQ(bin_op_const_versions->Oldest(), bin_op_const_versions->Newest());
  EXPECT_EQ(bin_op_const_versions->Oldest(), bin_op_const_versions->At(fidl::Version::Head()));
  EXPECT_SUBSTR(bin_op_const_versions->Oldest()->GetName(), "BIN_OP");

  auto raw_bin_op_type_ctor = std::move(raw_bin_op_const->type_ctor);
  auto bin_op_type_ctor_unique =
      source_map.GetUnique<fidl::flat::TypeConstructor>(raw_bin_op_type_ctor->source_signature());
  EXPECT_NOT_NULL(bin_op_type_ctor_unique);
  EXPECT_STREQ(bin_op_type_ctor_unique->Get()->type->name.decl_name().data(), "uint8");

  auto raw_bin_op_constant =
      static_cast<fidl::raw::BinaryOperatorConstant*>(raw_bin_op_const->constant.get());
  auto bin_op_constant_unique =
      source_map.GetUnique<fidl::flat::Constant>(raw_bin_op_constant->source_signature());
  EXPECT_NOT_NULL(bin_op_constant_unique);
  EXPECT_NOT_NULL(bin_op_constant_unique);
  EXPECT_EQ(static_cast<const fidl::flat::NumericConstantValue<uint8_t>*>(
                &bin_op_constant_unique->Get()->Value())
                ->value,
            3);

  auto raw_left_operand_constant = std::move(raw_bin_op_constant->left_operand);
  auto left_operand_constant_unique =
      source_map.GetUnique<fidl::flat::Constant>(raw_left_operand_constant->source_signature());
  EXPECT_NOT_NULL(left_operand_constant_unique);
  EXPECT_NOT_NULL(left_operand_constant_unique);
  EXPECT_EQ(static_cast<const fidl::flat::NumericConstantValue<uint8_t>*>(
                &left_operand_constant_unique->Get()->Value())
                ->value,
            2);

  auto raw_right_operand_constant = std::move(raw_bin_op_constant->right_operand);
  auto right_operand_constant_unique =
      source_map.GetUnique<fidl::flat::Constant>(raw_right_operand_constant->source_signature());
  EXPECT_NOT_NULL(right_operand_constant_unique);
  EXPECT_NOT_NULL(right_operand_constant_unique);
  EXPECT_EQ(static_cast<const fidl::flat::NumericConstantValue<uint8_t>*>(
                &right_operand_constant_unique->Get()->Value())
                ->value,
            1);
}

TEST(SourceMapGeneratorTests, GoodEnumUnversioned) {
  TestLibrary library(R"FIDL(library example;

type Unversioned = enum : uint32 {
  MEMBER = 1;
};

)FIDL");
  ASSERT_COMPILED(library);
  std::unique_ptr<fidl::raw::File> ast;
  ASSERT_TRUE(library.Parse(&ast));

  auto source_map = library.GenerateSourceMap("example");
  EXPECT_EQ(source_map.size(), 4);

  auto raw_type_decl = std::move(ast->type_decls[0]);
  auto raw_inline_ref =
      static_cast<fidl::raw::InlineLayoutReference*>(raw_type_decl->type_ctor->layout_ref.get());
  auto raw_layout = std::move(raw_inline_ref->layout);
  auto enum_versions = source_map.GetVersioned<fidl::flat::Enum>(raw_layout->source_signature());
  EXPECT_EQ(enum_versions->size(), 1);
  EXPECT_EQ(enum_versions->Oldest(), enum_versions->Newest());
  EXPECT_EQ(enum_versions->Oldest(), enum_versions->At(fidl::Version::Head()));
  EXPECT_SUBSTR(enum_versions->Oldest()->GetName(), "Unversioned");

  auto raw_subtype_ctor = std::move(raw_layout->subtype_ctor);
  auto subtype_ctor_unique =
      source_map.GetUnique<fidl::flat::TypeConstructor>(raw_subtype_ctor->source_signature());
  EXPECT_NOT_NULL(subtype_ctor_unique);
  EXPECT_EQ(subtype_ctor_unique->Get()->type->kind, fidl::flat::Type::Kind::kPrimitive);

  auto raw_member = std::move(raw_layout->members[0]);
  auto raw_enum_member = static_cast<fidl::raw::ValueLayoutMember*>(raw_member.get());
  auto enum_member_versions =
      source_map.GetVersioned<fidl::flat::Enum::Member>(raw_enum_member->source_signature());
  EXPECT_EQ(enum_member_versions->size(), 1);
  EXPECT_EQ(enum_member_versions->Oldest(), enum_member_versions->Newest());
  EXPECT_EQ(enum_member_versions->Oldest(), enum_member_versions->At(fidl::Version::Head()));
  EXPECT_EQ(enum_member_versions->Oldest()->name.data(), "MEMBER");

  auto raw_constant = std::move(raw_enum_member->value);
  auto constant_unique =
      source_map.GetUnique<fidl::flat::Constant>(raw_constant->source_signature());
  EXPECT_NOT_NULL(constant_unique);
  EXPECT_EQ(static_cast<const fidl::flat::NumericConstantValue<uint32_t>*>(
                &constant_unique->Get()->Value())
                ->value,
            1);
}

TEST(SourceMapGeneratorTests, GoodEnumVersioned) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Versioned = flexible enum : uint32 {
  @available(removed=2)
  OLD = 1;
  @available(added=2)
  NEW = 2;
};

)FIDL");
  library.SelectVersion("example", "HEAD");
  ASSERT_COMPILED(library);
  std::unique_ptr<fidl::raw::File> ast;
  ASSERT_TRUE(library.Parse(&ast));

  auto source_map = library.GenerateSourceMap("example");

  auto raw_type_decl = std::move(ast->type_decls[0]);
  auto raw_inline_ref =
      static_cast<fidl::raw::InlineLayoutReference*>(raw_type_decl->type_ctor->layout_ref.get());
  auto raw_layout = std::move(raw_inline_ref->layout);
  auto enum_versions = source_map.GetVersioned<fidl::flat::Enum>(raw_layout->source_signature());
  EXPECT_EQ(enum_versions->size(), 2);
  EXPECT_NE(enum_versions->Oldest(), enum_versions->Newest());
  EXPECT_EQ(enum_versions->Oldest(), enum_versions->At(fidl::Version::From(1).value()));
  EXPECT_EQ(enum_versions->Newest(), enum_versions->At(fidl::Version::From(2).value()));
  EXPECT_SUBSTR(enum_versions->Oldest()->GetName(), "Versioned");

  auto raw_subtype_ctor = std::move(raw_layout->subtype_ctor);
  auto subtype_ctor_unique =
      source_map.GetUnique<fidl::flat::TypeConstructor>(raw_subtype_ctor->source_signature());
  EXPECT_NOT_NULL(subtype_ctor_unique);
  EXPECT_EQ(subtype_ctor_unique->Get()->type->kind, fidl::flat::Type::Kind::kPrimitive);

  auto raw_removed_member = std::move(raw_layout->members[0]);
  auto raw_removed_enum_member =
      static_cast<fidl::raw::ValueLayoutMember*>(raw_removed_member.get());
  auto enum_removed_member_versions = source_map.GetVersioned<fidl::flat::Enum::Member>(
      raw_removed_enum_member->source_signature());
  EXPECT_EQ(enum_removed_member_versions->size(), 1);
  EXPECT_EQ(enum_removed_member_versions->Oldest(), enum_removed_member_versions->Newest());
  EXPECT_EQ(enum_removed_member_versions->Oldest(),
            enum_removed_member_versions->At(fidl::Version::From(1).value()));
  EXPECT_EQ(enum_removed_member_versions->Oldest()->name.data(), "OLD");

  auto raw_removed_constant = std::move(raw_removed_enum_member->value);
  auto removed_constant_unique =
      source_map.GetUnique<fidl::flat::Constant>(raw_removed_constant->source_signature());
  EXPECT_NOT_NULL(removed_constant_unique);
  EXPECT_EQ(static_cast<const fidl::flat::NumericConstantValue<uint32_t>*>(
                &removed_constant_unique->Get()->Value())
                ->value,
            1);

  auto raw_added_member = std::move(raw_layout->members[1]);
  auto raw_added_enum_member = static_cast<fidl::raw::ValueLayoutMember*>(raw_added_member.get());
  auto enum_added_member_versions =
      source_map.GetVersioned<fidl::flat::Enum::Member>(raw_added_enum_member->source_signature());
  EXPECT_EQ(enum_added_member_versions->size(), 1);
  EXPECT_EQ(enum_added_member_versions->Oldest(), enum_added_member_versions->Newest());
  EXPECT_EQ(enum_added_member_versions->Oldest(),
            enum_added_member_versions->At(fidl::Version::From(2).value()));
  EXPECT_EQ(enum_added_member_versions->Oldest()->name.data(), "NEW");

  auto raw_added_constant = std::move(raw_added_enum_member->value);
  auto added_constant_unique =
      source_map.GetUnique<fidl::flat::Constant>(raw_added_constant->source_signature());
  EXPECT_NOT_NULL(added_constant_unique);
  EXPECT_EQ(static_cast<const fidl::flat::NumericConstantValue<uint32_t>*>(
                &added_constant_unique->Get()->Value())
                ->value,
            2);
}

TEST(SourceMapGeneratorTests, GoodProtocolUnversioned) {
  TestLibrary library(R"FIDL(library example;

protocol Unversioned {
    Method(table {}) -> (table {});
};

)FIDL");
  ASSERT_COMPILED(library);
  std::unique_ptr<fidl::raw::File> ast;
  ASSERT_TRUE(library.Parse(&ast));

  auto source_map = library.GenerateSourceMap("example");
  EXPECT_EQ(source_map.size(), 6);

  auto raw_type_decl = std::move(ast->protocol_declaration_list[0]);
  auto raw_protocol_decl = static_cast<fidl::raw::ProtocolDeclaration*>(raw_type_decl.get());
  auto protocol_versions =
      source_map.GetVersioned<fidl::flat::Protocol>(raw_protocol_decl->source_signature());
  EXPECT_EQ(protocol_versions->size(), 1);
  EXPECT_EQ(protocol_versions->Oldest(), protocol_versions->Newest());
  EXPECT_EQ(protocol_versions->Oldest(), protocol_versions->At(fidl::Version::Head()));
  EXPECT_SUBSTR(protocol_versions->Oldest()->GetName(), "Unversioned");

  auto raw_member = std::move(raw_protocol_decl->methods[0]);
  auto raw_method = static_cast<fidl::raw::ProtocolMethod*>(raw_member.get());
  auto method_versions =
      source_map.GetVersioned<fidl::flat::Protocol::Method>(raw_method->source_signature());
  EXPECT_EQ(method_versions->size(), 1);
  EXPECT_EQ(method_versions->Oldest(), method_versions->Newest());
  EXPECT_EQ(method_versions->Oldest(), method_versions->At(fidl::Version::Head()));
  EXPECT_EQ(method_versions->Oldest()->name.data(), "Method");

  auto raw_request_type_ctor = std::move(raw_method->maybe_request->type_ctor);
  auto request_type_ctor_unique =
      source_map.GetUnique<fidl::flat::TypeConstructor>(raw_request_type_ctor->source_signature());
  EXPECT_NOT_NULL(request_type_ctor_unique);
  EXPECT_EQ(request_type_ctor_unique->Get()->type->kind, fidl::flat::Type::Kind::kIdentifier);

  auto raw_response_type_ctor = std::move(raw_method->maybe_response->type_ctor);
  auto response_type_ctor_unique =
      source_map.GetUnique<fidl::flat::TypeConstructor>(raw_response_type_ctor->source_signature());
  EXPECT_NOT_NULL(response_type_ctor_unique);
  EXPECT_EQ(response_type_ctor_unique->Get()->type->kind, fidl::flat::Type::Kind::kIdentifier);
}

TEST(SourceMapGeneratorTests, GoodProtocolVersioned) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

protocol Child1 {};
protocol Child2 {};

protocol Versioned {
  @available(removed=2)
  compose Child1;
  @available(added=2)
  compose Child2;

  @available(removed=2)
  MethodOld(table {}) -> (table {});
  @available(added=2)
  MethodNew() -> (table {}) error uint32;
};

)FIDL");
  library.SelectVersion("example", "HEAD");
  ASSERT_COMPILED(library);
  std::unique_ptr<fidl::raw::File> ast;
  ASSERT_TRUE(library.Parse(&ast));

  auto source_map = library.GenerateSourceMap("example");

  // Start from index 2, as 0 and 1 are for the unversioned |Child1| and |Child2|.
  auto raw_type_decl = std::move(ast->protocol_declaration_list[2]);
  auto raw_protocol_decl = static_cast<fidl::raw::ProtocolDeclaration*>(raw_type_decl.get());
  auto protocol_versions =
      source_map.GetVersioned<fidl::flat::Protocol>(raw_protocol_decl->source_signature());
  EXPECT_EQ(protocol_versions->size(), 2);
  EXPECT_NE(protocol_versions->Oldest(), protocol_versions->Newest());
  EXPECT_SUBSTR(protocol_versions->Oldest()->GetName(), "Versioned");
  EXPECT_EQ(protocol_versions->Oldest()->availability.range().pair().first, fidl::Version::From(1));
  EXPECT_SUBSTR(protocol_versions->Newest()->GetName(), "Versioned");
  EXPECT_EQ(protocol_versions->Newest()->availability.range().pair().first, fidl::Version::From(2));

  auto raw_removed_compose = std::move(raw_protocol_decl->composed_protocols[0]);
  auto removed_compose_versions = source_map.GetVersioned<fidl::flat::Protocol::ComposedProtocol>(
      raw_removed_compose->source_signature());
  EXPECT_EQ(removed_compose_versions->size(), 1);
  EXPECT_EQ(removed_compose_versions->Oldest(), removed_compose_versions->Newest());
  EXPECT_EQ(removed_compose_versions->Oldest(),
            removed_compose_versions->At(fidl::Version::From(1).value()));
  EXPECT_SUBSTR(removed_compose_versions->Oldest()->reference.span().data(), "Child1");

  auto raw_removed_method = std::move(raw_protocol_decl->methods[0]);
  auto removed_method_versions =
      source_map.GetVersioned<fidl::flat::Protocol::Method>(raw_removed_method->source_signature());
  EXPECT_EQ(removed_method_versions->size(), 1);
  EXPECT_EQ(removed_method_versions->Oldest(), removed_method_versions->Newest());
  EXPECT_EQ(removed_method_versions->Oldest(),
            removed_method_versions->At(fidl::Version::From(1).value()));
  EXPECT_EQ(removed_method_versions->Oldest()->name.data(), "MethodOld");
  EXPECT_EQ(removed_method_versions->Oldest()->availability.range().pair().first,
            fidl::Version::From(1));

  auto raw_removed_request_type_ctor = std::move(raw_removed_method->maybe_request->type_ctor);
  auto removed_request_type_ctor_unique = source_map.GetUnique<fidl::flat::TypeConstructor>(
      raw_removed_request_type_ctor->source_signature());
  EXPECT_NOT_NULL(removed_request_type_ctor_unique);
  EXPECT_EQ(removed_request_type_ctor_unique->Get()->type->kind,
            fidl::flat::Type::Kind::kIdentifier);

  auto raw_removed_response_type_ctor = std::move(raw_removed_method->maybe_response->type_ctor);
  auto removed_response_type_ctor_unique = source_map.GetUnique<fidl::flat::TypeConstructor>(
      raw_removed_response_type_ctor->source_signature());
  EXPECT_NOT_NULL(removed_response_type_ctor_unique);
  EXPECT_EQ(removed_response_type_ctor_unique->Get()->type->kind,
            fidl::flat::Type::Kind::kIdentifier);

  auto raw_added_compose = std::move(raw_protocol_decl->composed_protocols[1]);
  auto added_compose_versions = source_map.GetVersioned<fidl::flat::Protocol::ComposedProtocol>(
      raw_added_compose->source_signature());
  EXPECT_EQ(added_compose_versions->size(), 1);
  EXPECT_EQ(added_compose_versions->Oldest(), added_compose_versions->Newest());
  EXPECT_EQ(added_compose_versions->Oldest(),
            added_compose_versions->At(fidl::Version::From(2).value()));
  EXPECT_SUBSTR(added_compose_versions->Oldest()->reference.span().data(), "Child2");

  auto raw_added_method = std::move(raw_protocol_decl->methods[1]);
  auto added_method_versions =
      source_map.GetVersioned<fidl::flat::Protocol::Method>(raw_added_method->source_signature());
  EXPECT_EQ(added_method_versions->size(), 1);
  EXPECT_EQ(added_method_versions->Oldest(), added_method_versions->Newest());
  EXPECT_EQ(added_method_versions->Oldest(),
            added_method_versions->At(fidl::Version::From(2).value()));
  EXPECT_EQ(added_method_versions->Oldest()->name.data(), "MethodNew");
  EXPECT_EQ(added_method_versions->Oldest()->availability.range().pair().first,
            fidl::Version::From(2));

  auto raw_added_response_type_ctor = std::move(raw_added_method->maybe_response->type_ctor);
  auto added_response_type_ctor_unique = source_map.GetUnique<fidl::flat::TypeConstructor>(
      raw_added_response_type_ctor->source_signature());
  EXPECT_NOT_NULL(added_response_type_ctor_unique);

  auto raw_added_error_type_ctor = std::move(raw_added_method->maybe_error_ctor);
  auto added_error_type_ctor_unique = source_map.GetUnique<fidl::flat::TypeConstructor>(
      raw_added_error_type_ctor->source_signature());
  EXPECT_NOT_NULL(added_error_type_ctor_unique);
}

TEST(SourceMapGeneratorTests, GoodResourceUnversioned) {
  TestLibrary library(R"FIDL(library example;

protocol P {};

resource_definition Unversioned : uint32 {
  properties {
    subtype flexible enum {};
  };
};

)FIDL");
  ASSERT_COMPILED(library);
  std::unique_ptr<fidl::raw::File> ast;
  ASSERT_TRUE(library.Parse(&ast));

  auto source_map = library.GenerateSourceMap("example");
  EXPECT_EQ(source_map.size(), 6);

  auto raw_type_decl = std::move(ast->resource_declaration_list[0]);
  auto raw_resource_decl = static_cast<fidl::raw::ResourceDeclaration*>(raw_type_decl.get());
  auto resource_versions =
      source_map.GetVersioned<fidl::flat::Resource>(raw_resource_decl->source_signature());
  EXPECT_EQ(resource_versions->size(), 1);
  EXPECT_EQ(resource_versions->Oldest(), resource_versions->Newest());
  EXPECT_EQ(resource_versions->Oldest(), resource_versions->At(fidl::Version::Head()));
  EXPECT_EQ(resource_versions->Oldest(), resource_versions->At(fidl::Version::Head()));
  EXPECT_SUBSTR(resource_versions->Oldest()->GetName(), "Unversioned");

  auto raw_subtype_ctor = std::move(raw_resource_decl->maybe_type_ctor);
  auto subtype_ctor_unique =
      source_map.GetUnique<fidl::flat::TypeConstructor>(raw_subtype_ctor->source_signature());
  EXPECT_NOT_NULL(subtype_ctor_unique);
  EXPECT_EQ(subtype_ctor_unique->Get()->type->kind, fidl::flat::Type::Kind::kPrimitive);

  auto raw_property = std::move(raw_resource_decl->properties[0]);
  auto raw_resource_property = static_cast<fidl::raw::ResourceProperty*>(raw_property.get());
  auto resource_property_versions = source_map.GetVersioned<fidl::flat::Resource::Property>(
      raw_resource_property->source_signature());
  EXPECT_EQ(resource_property_versions->size(), 1);
  EXPECT_EQ(resource_property_versions->Oldest(), resource_property_versions->Newest());
  EXPECT_EQ(resource_property_versions->Oldest(),
            resource_property_versions->At(fidl::Version::Head()));
  EXPECT_EQ(resource_property_versions->Oldest()->name.data(), "subtype");

  auto raw_type_ctor = std::move(raw_resource_property->type_ctor);
  auto type_ctor_unique =
      source_map.GetUnique<fidl::flat::TypeConstructor>(raw_type_ctor->source_signature());
  EXPECT_NOT_NULL(type_ctor_unique);
  EXPECT_EQ(type_ctor_unique->Get()->type->kind, fidl::flat::Type::Kind::kIdentifier);
}

TEST(SourceMapGeneratorTests, GoodResourceVersioned) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

resource_definition Versioned : uint32 {
  properties {
    @available(removed=2)
    subtype flexible enum {};
    @available(added=2)
    subtype flexible enum {
        FOO = 1;
    };
  };
};

)FIDL");
  library.SelectVersion("example", "HEAD");
  ASSERT_COMPILED(library);
  std::unique_ptr<fidl::raw::File> ast;
  ASSERT_TRUE(library.Parse(&ast));

  auto source_map = library.GenerateSourceMap("example");

  auto raw_type_decl = std::move(ast->resource_declaration_list[0]);
  auto raw_resource_decl = static_cast<fidl::raw::ResourceDeclaration*>(raw_type_decl.get());
  auto resource_versions =
      source_map.GetVersioned<fidl::flat::Resource>(raw_resource_decl->source_signature());
  EXPECT_EQ(resource_versions->size(), 2);
  EXPECT_NE(resource_versions->Oldest(), resource_versions->Newest());
  EXPECT_EQ(resource_versions->Oldest(), resource_versions->At(fidl::Version::From(1).value()));
  EXPECT_EQ(resource_versions->Newest(), resource_versions->At(fidl::Version::From(2).value()));
  EXPECT_SUBSTR(resource_versions->Oldest()->GetName(), "Versioned");
  EXPECT_EQ(resource_versions->Oldest()->availability.range().pair().first, fidl::Version::From(1));
  EXPECT_SUBSTR(resource_versions->Newest()->GetName(), "Versioned");
  EXPECT_EQ(resource_versions->Newest()->availability.range().pair().first, fidl::Version::From(2));

  auto raw_subtype_ctor = std::move(raw_resource_decl->maybe_type_ctor);
  auto subtype_ctor_unique =
      source_map.GetUnique<fidl::flat::TypeConstructor>(raw_subtype_ctor->source_signature());
  EXPECT_NOT_NULL(subtype_ctor_unique);
  EXPECT_EQ(subtype_ctor_unique->Get()->type->kind, fidl::flat::Type::Kind::kPrimitive);

  auto raw_removed_property = std::move(raw_resource_decl->properties[0]);
  auto raw_removed_resource_property =
      static_cast<fidl::raw::ResourceProperty*>(raw_removed_property.get());
  auto resource_removed_property_versions = source_map.GetVersioned<fidl::flat::Resource::Property>(
      raw_removed_resource_property->source_signature());
  EXPECT_EQ(resource_removed_property_versions->size(), 1);
  EXPECT_EQ(resource_removed_property_versions->Oldest(),
            resource_removed_property_versions->Newest());
  EXPECT_EQ(resource_removed_property_versions->Oldest(),
            resource_removed_property_versions->At(fidl::Version::From(1).value()));
  EXPECT_EQ(resource_removed_property_versions->Oldest()->name.data(), "subtype");

  auto raw_removed_type_ctor = std::move(raw_removed_resource_property->type_ctor);
  auto removed_type_ctor_unique =
      source_map.GetUnique<fidl::flat::TypeConstructor>(raw_removed_type_ctor->source_signature());
  EXPECT_NOT_NULL(removed_type_ctor_unique);
  EXPECT_EQ(removed_type_ctor_unique->Get()->type->kind, fidl::flat::Type::Kind::kIdentifier);

  auto raw_added_property = std::move(raw_resource_decl->properties[1]);
  auto raw_added_resource_property =
      static_cast<fidl::raw::ResourceProperty*>(raw_added_property.get());
  auto resource_added_property_versions = source_map.GetVersioned<fidl::flat::Resource::Property>(
      raw_added_resource_property->source_signature());
  EXPECT_EQ(resource_added_property_versions->size(), 1);
  EXPECT_EQ(resource_added_property_versions->Oldest(), resource_added_property_versions->Newest());
  EXPECT_EQ(resource_added_property_versions->Oldest(),
            resource_added_property_versions->At(fidl::Version::From(2).value()));
  EXPECT_EQ(resource_added_property_versions->Oldest()->name.data(), "subtype");

  auto raw_added_type_ctor = std::move(raw_added_resource_property->type_ctor);
  auto added_type_ctor_unique =
      source_map.GetUnique<fidl::flat::TypeConstructor>(raw_added_type_ctor->source_signature());
  EXPECT_NOT_NULL(added_type_ctor_unique);
  EXPECT_EQ(added_type_ctor_unique->Get()->type->kind, fidl::flat::Type::Kind::kIdentifier);
}

TEST(SourceMapGeneratorTests, GoodServiceUnversioned) {
  TestLibrary library(R"FIDL(library example;

protocol P {};

service Unversioned {
  f1 client_end:<P>;
};

)FIDL");
  ASSERT_COMPILED(library);
  std::unique_ptr<fidl::raw::File> ast;
  ASSERT_TRUE(library.Parse(&ast));

  auto source_map = library.GenerateSourceMap("example");
  EXPECT_EQ(source_map.size(), 6);

  auto raw_type_decl = std::move(ast->service_declaration_list[0]);
  auto raw_service_decl = static_cast<fidl::raw::ServiceDeclaration*>(raw_type_decl.get());
  auto service_versions =
      source_map.GetVersioned<fidl::flat::Service>(raw_service_decl->source_signature());
  EXPECT_EQ(service_versions->size(), 1);
  EXPECT_EQ(service_versions->Oldest(), service_versions->Newest());
  EXPECT_EQ(service_versions->Oldest(), service_versions->At(fidl::Version::Head()));
  EXPECT_SUBSTR(service_versions->Oldest()->GetName(), "Unversioned");

  auto raw_member = std::move(raw_service_decl->members[0]);
  auto raw_service_member = static_cast<fidl::raw::ServiceMember*>(raw_member.get());
  auto service_member_versions =
      source_map.GetVersioned<fidl::flat::Service::Member>(raw_service_member->source_signature());
  EXPECT_EQ(service_member_versions->size(), 1);
  EXPECT_EQ(service_member_versions->Oldest(), service_member_versions->Newest());
  EXPECT_EQ(service_member_versions->Oldest(), service_member_versions->At(fidl::Version::Head()));
  EXPECT_EQ(service_member_versions->Oldest()->name.data(), "f1");

  auto raw_type_ctor = std::move(raw_service_member->type_ctor);
  auto type_ctor_unique =
      source_map.GetUnique<fidl::flat::TypeConstructor>(raw_type_ctor->source_signature());
  EXPECT_NOT_NULL(type_ctor_unique);
  EXPECT_EQ(type_ctor_unique->Get()->type->kind, fidl::flat::Type::Kind::kTransportSide);
}

TEST(SourceMapGeneratorTests, GoodServiceVersioned) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

protocol P {};

service Versioned {
  @available(removed=2)
  f1 client_end:<P>;
  @available(added=2)
  f2 client_end:<P>;
};

)FIDL");
  library.SelectVersion("example", "HEAD");
  ASSERT_COMPILED(library);
  std::unique_ptr<fidl::raw::File> ast;
  ASSERT_TRUE(library.Parse(&ast));

  auto source_map = library.GenerateSourceMap("example");

  auto raw_type_decl = std::move(ast->service_declaration_list[0]);
  auto raw_service_decl = static_cast<fidl::raw::ServiceDeclaration*>(raw_type_decl.get());
  auto service_versions =
      source_map.GetVersioned<fidl::flat::Service>(raw_service_decl->source_signature());
  EXPECT_EQ(service_versions->size(), 2);
  EXPECT_NE(service_versions->Oldest(), service_versions->Newest());
  EXPECT_SUBSTR(service_versions->Oldest()->GetName(), "Versioned");
  EXPECT_EQ(service_versions->Oldest()->availability.range().pair().first, fidl::Version::From(1));
  EXPECT_SUBSTR(service_versions->Newest()->GetName(), "Versioned");
  EXPECT_EQ(service_versions->Newest()->availability.range().pair().first, fidl::Version::From(2));

  auto raw_removed_member = std::move(raw_service_decl->members[0]);
  auto raw_removed_service_member =
      static_cast<fidl::raw::ServiceMember*>(raw_removed_member.get());
  auto service_removed_member_versions = source_map.GetVersioned<fidl::flat::Service::Member>(
      raw_removed_service_member->source_signature());
  EXPECT_EQ(service_removed_member_versions->size(), 1);
  EXPECT_EQ(service_removed_member_versions->Oldest(), service_removed_member_versions->Newest());
  EXPECT_EQ(service_removed_member_versions->Oldest(),
            service_removed_member_versions->At(fidl::Version::From(1).value()));
  EXPECT_EQ(service_removed_member_versions->Oldest()->name.data(), "f1");

  auto raw_removed_type_ctor = std::move(raw_removed_service_member->type_ctor);
  auto removed_type_ctor_unique =
      source_map.GetUnique<fidl::flat::TypeConstructor>(raw_removed_type_ctor->source_signature());
  EXPECT_NOT_NULL(removed_type_ctor_unique);
  EXPECT_EQ(removed_type_ctor_unique->Get()->type->kind, fidl::flat::Type::Kind::kTransportSide);

  auto raw_added_member = std::move(raw_service_decl->members[1]);
  auto raw_added_service_member = static_cast<fidl::raw::ServiceMember*>(raw_added_member.get());
  auto service_added_member_versions = source_map.GetVersioned<fidl::flat::Service::Member>(
      raw_added_service_member->source_signature());
  EXPECT_EQ(service_added_member_versions->size(), 1);
  EXPECT_EQ(service_added_member_versions->Oldest(), service_added_member_versions->Newest());
  EXPECT_EQ(service_added_member_versions->Oldest(),
            service_added_member_versions->At(fidl::Version::From(2).value()));
  EXPECT_EQ(service_added_member_versions->Oldest()->name.data(), "f2");

  auto raw_added_type_ctor = std::move(raw_added_service_member->type_ctor);
  auto added_type_ctor_unique =
      source_map.GetUnique<fidl::flat::TypeConstructor>(raw_added_type_ctor->source_signature());
  EXPECT_NOT_NULL(added_type_ctor_unique);
  EXPECT_EQ(added_type_ctor_unique->Get()->type->kind, fidl::flat::Type::Kind::kTransportSide);
}

TEST(SourceMapGeneratorTests, GoodStructUnversioned) {
  TestLibrary library(R"FIDL(library example;

type Unversioned = struct {
  f1 string;
};

)FIDL");
  ASSERT_COMPILED(library);
  std::unique_ptr<fidl::raw::File> ast;
  ASSERT_TRUE(library.Parse(&ast));

  auto source_map = library.GenerateSourceMap("example");
  EXPECT_EQ(source_map.size(), 3);

  auto raw_type_decl = std::move(ast->type_decls[0]);
  auto raw_inline_ref =
      static_cast<fidl::raw::InlineLayoutReference*>(raw_type_decl->type_ctor->layout_ref.get());
  auto raw_layout = std::move(raw_inline_ref->layout);
  auto struct_versions =
      source_map.GetVersioned<fidl::flat::Struct>(raw_layout->source_signature());
  EXPECT_EQ(struct_versions->size(), 1);
  EXPECT_EQ(struct_versions->Oldest(), struct_versions->Newest());
  EXPECT_EQ(struct_versions->Oldest(), struct_versions->At(fidl::Version::Head()));
  EXPECT_SUBSTR(struct_versions->Oldest()->GetName(), "Unversioned");

  auto raw_member = std::move(raw_layout->members[0]);
  auto raw_struct_member = static_cast<fidl::raw::StructLayoutMember*>(raw_member.get());
  auto struct_member_versions =
      source_map.GetVersioned<fidl::flat::Struct::Member>(raw_struct_member->source_signature());
  EXPECT_EQ(struct_member_versions->size(), 1);
  EXPECT_EQ(struct_member_versions->Oldest(), struct_member_versions->Newest());
  EXPECT_EQ(struct_member_versions->Oldest(), struct_member_versions->At(fidl::Version::Head()));
  EXPECT_EQ(struct_member_versions->Oldest()->name.data(), "f1");

  auto raw_type_ctor = std::move(raw_struct_member->type_ctor);
  auto type_ctor_unique =
      source_map.GetUnique<fidl::flat::TypeConstructor>(raw_type_ctor->source_signature());
  EXPECT_NOT_NULL(type_ctor_unique);
  EXPECT_EQ(type_ctor_unique->Get()->type->kind, fidl::flat::Type::Kind::kString);
}

TEST(SourceMapGeneratorTests, GoodStructVersioned) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Versioned = struct {
  inner struct {
    @available(removed=2)
    f1 string;
    @available(added=2)
    f2 bool;
  };
};

)FIDL");
  library.SelectVersion("example", "HEAD");
  ASSERT_COMPILED(library);
  std::unique_ptr<fidl::raw::File> ast;
  ASSERT_TRUE(library.Parse(&ast));

  auto source_map = library.GenerateSourceMap("example");

  auto raw_type_decl = std::move(ast->type_decls[0]);
  auto raw_outer_inline_ref =
      static_cast<fidl::raw::InlineLayoutReference*>(raw_type_decl->type_ctor->layout_ref.get());
  auto raw_outer_layout = std::move(raw_outer_inline_ref->layout);
  auto outer_struct_versions =
      source_map.GetVersioned<fidl::flat::Struct>(raw_outer_layout->source_signature());
  EXPECT_EQ(outer_struct_versions->size(), 2);
  EXPECT_NE(outer_struct_versions->Oldest(), outer_struct_versions->Newest());
  EXPECT_SUBSTR(outer_struct_versions->Oldest()->GetName(), "Versioned");
  EXPECT_EQ(outer_struct_versions->Oldest()->availability.range().pair().first,
            fidl::Version::From(1));
  EXPECT_SUBSTR(outer_struct_versions->Newest()->GetName(), "Versioned");
  EXPECT_EQ(outer_struct_versions->Newest()->availability.range().pair().first,
            fidl::Version::From(2));

  auto raw_outer_member = std::move(raw_outer_layout->members[0]);
  auto raw_outer_struct_member =
      static_cast<fidl::raw::StructLayoutMember*>(raw_outer_member.get());
  auto struct_member_versions = source_map.GetVersioned<fidl::flat::Struct::Member>(
      raw_outer_struct_member->source_signature());
  EXPECT_EQ(struct_member_versions->size(), 2);
  EXPECT_NE(struct_member_versions->Oldest(), struct_member_versions->Newest());
  EXPECT_EQ(struct_member_versions->Oldest(),
            struct_member_versions->At(fidl::Version::From(1).value()));
  EXPECT_EQ(struct_member_versions->Newest(),
            struct_member_versions->At(fidl::Version::From(2).value()));
  EXPECT_STREQ(struct_member_versions->Oldest()->name.data(), "inner");
  EXPECT_EQ(struct_member_versions->Oldest()->availability.range().pair().first,
            fidl::Version::From(1));
  EXPECT_STREQ(struct_member_versions->Newest()->name.data(), "inner");
  EXPECT_EQ(struct_member_versions->Newest()->availability.range().pair().first,
            fidl::Version::From(2));

  auto raw_outer_type_ctor = std::move(raw_outer_struct_member->type_ctor);
  auto outer_type_ctor_unique =
      source_map.GetUnique<fidl::flat::TypeConstructor>(raw_outer_type_ctor->source_signature());
  EXPECT_NOT_NULL(outer_type_ctor_unique);
  EXPECT_EQ(outer_type_ctor_unique->Get()->type->kind, fidl::flat::Type::Kind::kIdentifier);

  auto raw_inner_inline_ref =
      static_cast<fidl::raw::InlineLayoutReference*>(raw_outer_type_ctor->layout_ref.get());
  auto raw_inner_layout = std::move(raw_inner_inline_ref->layout);
  auto inner_struct_versions =
      source_map.GetVersioned<fidl::flat::Struct>(raw_inner_layout->source_signature());
  EXPECT_EQ(inner_struct_versions->size(), 2);
  EXPECT_NE(inner_struct_versions->Oldest(), inner_struct_versions->Newest());
  EXPECT_EQ(inner_struct_versions->Oldest(),
            inner_struct_versions->At(fidl::Version::From(1).value()));
  EXPECT_EQ(inner_struct_versions->Newest(),
            inner_struct_versions->At(fidl::Version::From(2).value()));

  auto raw_removed_member = std::move(raw_inner_layout->members[0]);
  auto raw_removed_struct_member =
      static_cast<fidl::raw::StructLayoutMember*>(raw_removed_member.get());
  auto struct_removed_member_versions = source_map.GetVersioned<fidl::flat::Struct::Member>(
      raw_removed_struct_member->source_signature());
  EXPECT_EQ(struct_removed_member_versions->size(), 1);
  EXPECT_EQ(struct_removed_member_versions->Oldest(), struct_removed_member_versions->Newest());
  EXPECT_EQ(struct_removed_member_versions->Oldest(),
            struct_removed_member_versions->At(fidl::Version::From(1).value()));
  EXPECT_EQ(struct_removed_member_versions->Oldest()->name.data(), "f1");

  auto raw_removed_type_ctor = std::move(raw_removed_struct_member->type_ctor);
  auto removed_type_ctor_unique =
      source_map.GetUnique<fidl::flat::TypeConstructor>(raw_removed_type_ctor->source_signature());
  EXPECT_NOT_NULL(removed_type_ctor_unique);
  EXPECT_EQ(removed_type_ctor_unique->Get()->type->kind, fidl::flat::Type::Kind::kString);

  auto raw_added_member = std::move(raw_inner_layout->members[1]);
  auto raw_added_struct_member =
      static_cast<fidl::raw::StructLayoutMember*>(raw_added_member.get());
  auto struct_added_member_versions = source_map.GetVersioned<fidl::flat::Struct::Member>(
      raw_added_struct_member->source_signature());
  EXPECT_EQ(struct_added_member_versions->size(), 1);
  EXPECT_EQ(struct_added_member_versions->Oldest(), struct_added_member_versions->Newest());
  EXPECT_EQ(struct_added_member_versions->Oldest(),
            struct_added_member_versions->At(fidl::Version::From(2).value()));
  EXPECT_EQ(struct_added_member_versions->Oldest()->name.data(), "f2");

  auto raw_added_type_ctor = std::move(raw_added_struct_member->type_ctor);
  auto added_type_ctor_unique =
      source_map.GetUnique<fidl::flat::TypeConstructor>(raw_added_type_ctor->source_signature());
  EXPECT_NOT_NULL(added_type_ctor_unique);
  EXPECT_EQ(added_type_ctor_unique->Get()->type->kind, fidl::flat::Type::Kind::kPrimitive);
}

TEST(SourceMapGeneratorTests, GoodTableUnversioned) {
  TestLibrary library(R"FIDL(library example;

type Unversioned = table {
  1: f1 string;
};

)FIDL");
  ASSERT_COMPILED(library);
  std::unique_ptr<fidl::raw::File> ast;
  ASSERT_TRUE(library.Parse(&ast));

  auto source_map = library.GenerateSourceMap("example");
  EXPECT_EQ(source_map.size(), 3);

  auto raw_type_decl = std::move(ast->type_decls[0]);
  auto raw_inline_ref =
      static_cast<fidl::raw::InlineLayoutReference*>(raw_type_decl->type_ctor->layout_ref.get());
  auto raw_layout = std::move(raw_inline_ref->layout);
  auto table_versions = source_map.GetVersioned<fidl::flat::Table>(raw_layout->source_signature());
  EXPECT_EQ(table_versions->size(), 1);
  EXPECT_EQ(table_versions->Oldest(), table_versions->Newest());
  EXPECT_EQ(table_versions->Oldest(), table_versions->At(fidl::Version::Head()));
  EXPECT_SUBSTR(table_versions->Oldest()->GetName(), "Unversioned");

  auto raw_member = std::move(raw_layout->members[0]);
  auto raw_table_member = static_cast<fidl::raw::OrdinaledLayoutMember*>(raw_member.get());
  auto table_member_versions =
      source_map.GetVersioned<fidl::flat::Table::Member>(raw_table_member->source_signature());
  EXPECT_EQ(table_member_versions->size(), 1);
  EXPECT_EQ(table_member_versions->Oldest(), table_member_versions->Newest());
  EXPECT_EQ(table_member_versions->Oldest(), table_member_versions->At(fidl::Version::Head()));
  EXPECT_EQ(table_member_versions->Oldest()->maybe_used->name.data(), "f1");

  auto raw_type_ctor = std::move(raw_table_member->type_ctor);
  auto type_ctor_unique =
      source_map.GetUnique<fidl::flat::TypeConstructor>(raw_type_ctor->source_signature());
  EXPECT_NOT_NULL(type_ctor_unique);
}

TEST(SourceMapGeneratorTests, GoodTableVersioned) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Versioned = table {
  1: inner table {
    @available(removed=2)
    1: f1 string;
    @available(added=2)
    1: f2 bool;
  };
};

)FIDL");
  library.SelectVersion("example", "HEAD");
  ASSERT_COMPILED(library);
  std::unique_ptr<fidl::raw::File> ast;
  ASSERT_TRUE(library.Parse(&ast));

  auto source_map = library.GenerateSourceMap("example");

  auto raw_type_decl = std::move(ast->type_decls[0]);
  auto raw_outer_inline_ref =
      static_cast<fidl::raw::InlineLayoutReference*>(raw_type_decl->type_ctor->layout_ref.get());
  auto raw_outer_layout = std::move(raw_outer_inline_ref->layout);
  auto outer_table_versions =
      source_map.GetVersioned<fidl::flat::Table>(raw_outer_layout->source_signature());
  EXPECT_EQ(outer_table_versions->size(), 2);
  EXPECT_NE(outer_table_versions->Oldest(), outer_table_versions->Newest());
  EXPECT_EQ(outer_table_versions->Oldest(),
            outer_table_versions->At(fidl::Version::From(1).value()));
  EXPECT_EQ(outer_table_versions->Newest(),
            outer_table_versions->At(fidl::Version::From(2).value()));
  EXPECT_SUBSTR(outer_table_versions->Oldest()->GetName(), "Versioned");
  EXPECT_EQ(outer_table_versions->Oldest()->availability.range().pair().first,
            fidl::Version::From(1));
  EXPECT_SUBSTR(outer_table_versions->Newest()->GetName(), "Versioned");
  EXPECT_EQ(outer_table_versions->Newest()->availability.range().pair().first,
            fidl::Version::From(2));

  auto raw_outer_member = std::move(raw_outer_layout->members[0]);
  auto raw_outer_table_member =
      static_cast<fidl::raw::OrdinaledLayoutMember*>(raw_outer_member.get());
  auto table_member_versions = source_map.GetVersioned<fidl::flat::Table::Member>(
      raw_outer_table_member->source_signature());
  EXPECT_EQ(table_member_versions->size(), 2);
  EXPECT_NE(table_member_versions->Oldest(), table_member_versions->Newest());
  EXPECT_EQ(table_member_versions->Oldest(),
            table_member_versions->At(fidl::Version::From(1).value()));
  EXPECT_EQ(table_member_versions->Newest(),
            table_member_versions->At(fidl::Version::From(2).value()));
  EXPECT_STREQ(table_member_versions->Oldest()->maybe_used->name.data(), "inner");
  EXPECT_EQ(table_member_versions->Oldest()->availability.range().pair().first,
            fidl::Version::From(1));
  EXPECT_STREQ(table_member_versions->Newest()->maybe_used->name.data(), "inner");
  EXPECT_EQ(table_member_versions->Newest()->availability.range().pair().first,
            fidl::Version::From(2));

  auto raw_outer_type_ctor = std::move(raw_outer_table_member->type_ctor);
  auto outer_type_ctor_unique =
      source_map.GetUnique<fidl::flat::TypeConstructor>(raw_outer_type_ctor->source_signature());
  EXPECT_NOT_NULL(outer_type_ctor_unique);
  EXPECT_EQ(outer_type_ctor_unique->Get()->type->kind, fidl::flat::Type::Kind::kIdentifier);

  auto raw_inner_inline_ref =
      static_cast<fidl::raw::InlineLayoutReference*>(raw_outer_type_ctor->layout_ref.get());
  auto raw_inner_layout = std::move(raw_inner_inline_ref->layout);
  auto inner_table_versions =
      source_map.GetVersioned<fidl::flat::Table>(raw_inner_layout->source_signature());
  EXPECT_EQ(inner_table_versions->size(), 2);
  EXPECT_NE(inner_table_versions->Oldest(), inner_table_versions->Newest());
  EXPECT_EQ(inner_table_versions->Oldest(),
            inner_table_versions->At(fidl::Version::From(1).value()));
  EXPECT_EQ(inner_table_versions->Newest(),
            inner_table_versions->At(fidl::Version::From(2).value()));

  auto raw_removed_member = std::move(raw_inner_layout->members[0]);
  auto raw_removed_table_member =
      static_cast<fidl::raw::OrdinaledLayoutMember*>(raw_removed_member.get());
  auto table_removed_member_versions = source_map.GetVersioned<fidl::flat::Table::Member>(
      raw_removed_table_member->source_signature());
  EXPECT_EQ(table_removed_member_versions->size(), 1);
  EXPECT_EQ(table_removed_member_versions->Oldest(),
            table_removed_member_versions->At(fidl::Version::From(1).value()));
  EXPECT_EQ(table_removed_member_versions->Oldest()->maybe_used->name.data(), "f1");

  auto raw_removed_type_ctor = std::move(raw_removed_table_member->type_ctor);
  auto removed_type_ctor_unique =
      source_map.GetUnique<fidl::flat::TypeConstructor>(raw_removed_type_ctor->source_signature());
  EXPECT_NOT_NULL(removed_type_ctor_unique);
  EXPECT_EQ(removed_type_ctor_unique->Get()->type->kind, fidl::flat::Type::Kind::kString);

  auto raw_added_member = std::move(raw_inner_layout->members[1]);
  auto raw_added_table_member =
      static_cast<fidl::raw::OrdinaledLayoutMember*>(raw_added_member.get());
  auto table_added_member_versions = source_map.GetVersioned<fidl::flat::Table::Member>(
      raw_added_table_member->source_signature());
  EXPECT_EQ(table_added_member_versions->size(), 1);
  EXPECT_EQ(table_added_member_versions->Oldest(), table_added_member_versions->Newest());
  EXPECT_EQ(table_added_member_versions->Oldest(),
            table_added_member_versions->At(fidl::Version::From(2).value()));
  EXPECT_EQ(table_added_member_versions->Oldest()->maybe_used->name.data(), "f2");

  auto raw_added_type_ctor = std::move(raw_added_table_member->type_ctor);
  auto added_type_ctor_unique =
      source_map.GetUnique<fidl::flat::TypeConstructor>(raw_added_type_ctor->source_signature());
  EXPECT_NOT_NULL(added_type_ctor_unique);
  EXPECT_EQ(added_type_ctor_unique->Get()->type->kind, fidl::flat::Type::Kind::kPrimitive);
}

TEST(SourceMapGeneratorTests, GoodUnionUnversioned) {
  TestLibrary library(R"FIDL(library example;

type Unversioned = union {
  1: f1 string;
};

)FIDL");
  ASSERT_COMPILED(library);
  std::unique_ptr<fidl::raw::File> ast;
  ASSERT_TRUE(library.Parse(&ast));

  auto source_map = library.GenerateSourceMap("example");
  EXPECT_EQ(source_map.size(), 3);

  auto raw_type_decl = std::move(ast->type_decls[0]);
  auto raw_inline_ref =
      static_cast<fidl::raw::InlineLayoutReference*>(raw_type_decl->type_ctor->layout_ref.get());
  auto raw_layout = std::move(raw_inline_ref->layout);
  auto union_versions = source_map.GetVersioned<fidl::flat::Union>(raw_layout->source_signature());
  EXPECT_EQ(union_versions->size(), 1);
  EXPECT_EQ(union_versions->Oldest(), union_versions->Newest());
  EXPECT_EQ(union_versions->Oldest(), union_versions->At(fidl::Version::Head()));
  EXPECT_SUBSTR(union_versions->Oldest()->GetName(), "Unversioned");

  auto raw_member = std::move(raw_layout->members[0]);
  auto raw_union_member = static_cast<fidl::raw::OrdinaledLayoutMember*>(raw_member.get());
  auto union_member_versions =
      source_map.GetVersioned<fidl::flat::Union::Member>(raw_union_member->source_signature());
  EXPECT_EQ(union_member_versions->size(), 1);
  EXPECT_EQ(union_member_versions->Oldest(), union_member_versions->Newest());
  EXPECT_EQ(union_member_versions->Oldest()->maybe_used->name.data(), "f1");

  auto raw_type_ctor = std::move(raw_union_member->type_ctor);
  auto type_ctor_unique =
      source_map.GetUnique<fidl::flat::TypeConstructor>(raw_type_ctor->source_signature());
  EXPECT_NOT_NULL(type_ctor_unique);
  EXPECT_EQ(type_ctor_unique->Get()->type->kind, fidl::flat::Type::Kind::kString);
}

TEST(SourceMapGeneratorTests, GoodUnionVersioned) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Versioned = union {
  1: inner union {
    @available(removed=2)
    1: f1 string;
    @available(added=2)
    1: f2 bool;
  };
};

)FIDL");
  library.SelectVersion("example", "HEAD");
  ASSERT_COMPILED(library);
  std::unique_ptr<fidl::raw::File> ast;
  ASSERT_TRUE(library.Parse(&ast));

  auto source_map = library.GenerateSourceMap("example");

  auto raw_type_decl = std::move(ast->type_decls[0]);
  auto raw_outer_inline_ref =
      static_cast<fidl::raw::InlineLayoutReference*>(raw_type_decl->type_ctor->layout_ref.get());
  auto raw_outer_layout = std::move(raw_outer_inline_ref->layout);
  auto outer_union_versions =
      source_map.GetVersioned<fidl::flat::Union>(raw_outer_layout->source_signature());
  EXPECT_EQ(outer_union_versions->size(), 2);
  EXPECT_NE(outer_union_versions->Oldest(), outer_union_versions->Newest());
  EXPECT_SUBSTR(outer_union_versions->Oldest()->GetName(), "Versioned");
  EXPECT_EQ(outer_union_versions->Oldest()->availability.range().pair().first,
            fidl::Version::From(1));
  EXPECT_SUBSTR(outer_union_versions->Newest()->GetName(), "Versioned");
  EXPECT_EQ(outer_union_versions->Newest()->availability.range().pair().first,
            fidl::Version::From(2));

  auto raw_outer_member = std::move(raw_outer_layout->members[0]);
  auto raw_outer_union_member =
      static_cast<fidl::raw::OrdinaledLayoutMember*>(raw_outer_member.get());
  auto union_member_versions = source_map.GetVersioned<fidl::flat::Union::Member>(
      raw_outer_union_member->source_signature());
  EXPECT_EQ(union_member_versions->size(), 2);
  EXPECT_NE(union_member_versions->Oldest(), union_member_versions->Newest());
  EXPECT_STREQ(union_member_versions->Oldest()->maybe_used->name.data(), "inner");
  EXPECT_EQ(union_member_versions->Oldest()->availability.range().pair().first,
            fidl::Version::From(1));
  EXPECT_STREQ(union_member_versions->Newest()->maybe_used->name.data(), "inner");
  EXPECT_EQ(union_member_versions->Newest()->availability.range().pair().first,
            fidl::Version::From(2));

  auto raw_outer_type_ctor = std::move(raw_outer_union_member->type_ctor);
  auto outer_type_ctor_unique =
      source_map.GetUnique<fidl::flat::TypeConstructor>(raw_outer_type_ctor->source_signature());
  EXPECT_NOT_NULL(outer_type_ctor_unique);
  EXPECT_EQ(outer_type_ctor_unique->Get()->type->kind, fidl::flat::Type::Kind::kIdentifier);

  auto raw_inner_inline_ref =
      static_cast<fidl::raw::InlineLayoutReference*>(raw_outer_type_ctor->layout_ref.get());
  auto raw_inner_layout = std::move(raw_inner_inline_ref->layout);
  auto inner_union_versions =
      source_map.GetVersioned<fidl::flat::Union>(raw_inner_layout->source_signature());
  EXPECT_EQ(inner_union_versions->size(), 2);
  EXPECT_NE(inner_union_versions->Oldest(), inner_union_versions->Newest());

  auto raw_removed_member = std::move(raw_inner_layout->members[0]);
  auto raw_removed_union_member =
      static_cast<fidl::raw::OrdinaledLayoutMember*>(raw_removed_member.get());
  auto union_removed_member_versions = source_map.GetVersioned<fidl::flat::Union::Member>(
      raw_removed_union_member->source_signature());
  EXPECT_EQ(union_removed_member_versions->size(), 1);
  EXPECT_EQ(union_removed_member_versions->Oldest(), union_removed_member_versions->Newest());
  EXPECT_EQ(union_removed_member_versions->Oldest()->maybe_used->name.data(), "f1");

  auto raw_removed_type_ctor = std::move(raw_removed_union_member->type_ctor);
  auto removed_type_ctor_unique =
      source_map.GetUnique<fidl::flat::TypeConstructor>(raw_removed_type_ctor->source_signature());
  EXPECT_NOT_NULL(removed_type_ctor_unique);
  EXPECT_EQ(removed_type_ctor_unique->Get()->type->kind, fidl::flat::Type::Kind::kString);

  auto raw_added_member = std::move(raw_inner_layout->members[1]);
  auto raw_added_union_member =
      static_cast<fidl::raw::OrdinaledLayoutMember*>(raw_added_member.get());
  auto union_added_member_versions = source_map.GetVersioned<fidl::flat::Union::Member>(
      raw_added_union_member->source_signature());
  EXPECT_EQ(union_added_member_versions->size(), 1);
  EXPECT_EQ(union_added_member_versions->Oldest(), union_added_member_versions->Newest());
  EXPECT_EQ(union_added_member_versions->Oldest()->maybe_used->name.data(), "f2");

  auto raw_added_type_ctor = std::move(raw_added_union_member->type_ctor);
  auto added_type_ctor_unique =
      source_map.GetUnique<fidl::flat::TypeConstructor>(raw_added_type_ctor->source_signature());
  EXPECT_NOT_NULL(added_type_ctor_unique);
  EXPECT_EQ(added_type_ctor_unique->Get()->type->kind, fidl::flat::Type::Kind::kPrimitive);
}

}  // namespace
