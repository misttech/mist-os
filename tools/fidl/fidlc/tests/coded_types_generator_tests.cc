// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zircon/assert.h>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "tools/fidl/fidlc/include/fidl/coded_ast.h"
#include "tools/fidl/fidlc/include/fidl/coded_types_generator.h"
#include "tools/fidl/fidlc/tests/test_library.h"

namespace {

using ::testing::HasSubstr;

const fidl::coded::StructField& field(const fidl::coded::StructElement& element) {
  ZX_ASSERT(std::holds_alternative<const fidl::coded::StructField>(element));
  return std::get<const fidl::coded::StructField>(element);
}
const fidl::coded::StructPadding& padding(const fidl::coded::StructElement& element) {
  ZX_ASSERT(std::holds_alternative<const fidl::coded::StructPadding>(element));
  return std::get<const fidl::coded::StructPadding>(element);
}

TEST(CodedTypesGeneratorTests, GoodCodedTypesOfArrays) {
  TestLibrary library(R"FIDL(library example;

type Arrays = struct {
    prime array<uint8, 7>;
    next_prime array<array<uint8, 7>, 11>;
    next_next_prime array<array<array<uint8, 7>, 11>, 13>;
};
)FIDL");
  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();

  ASSERT_EQ(4u, gen.coded_types().size());

  auto type0 = gen.coded_types().at(0).get();
  EXPECT_EQ("uint8", type0->coded_name);
  EXPECT_TRUE(type0->is_coding_needed);
  ASSERT_EQ(fidl::coded::Type::Kind::kPrimitive, type0->kind);
  auto type0_primitive = static_cast<const fidl::coded::PrimitiveType*>(type0);
  EXPECT_EQ(fidl::types::PrimitiveSubtype::kUint8, type0_primitive->subtype);

  auto type1 = gen.coded_types().at(1).get();
  EXPECT_EQ("Array7_5uint8", type1->coded_name);
  EXPECT_TRUE(type1->is_coding_needed);
  ASSERT_EQ(fidl::coded::Type::Kind::kArray, type1->kind);
  auto type1_array = static_cast<const fidl::coded::ArrayType*>(type1);
  EXPECT_EQ(1u, type1_array->element_size_v2);
  EXPECT_EQ(type0, type1_array->element_type);

  auto type2 = gen.coded_types().at(2).get();
  EXPECT_EQ("Array77_13Array7_5uint8", type2->coded_name);
  EXPECT_TRUE(type2->is_coding_needed);
  ASSERT_EQ(fidl::coded::Type::Kind::kArray, type2->kind);
  auto type2_array = static_cast<const fidl::coded::ArrayType*>(type2);
  EXPECT_EQ(7u * 1, type2_array->element_size_v2);
  EXPECT_EQ(type1, type2_array->element_type);

  auto type3 = gen.coded_types().at(3).get();
  EXPECT_EQ("Array1001_23Array77_13Array7_5uint8", type3->coded_name);
  EXPECT_TRUE(type3->is_coding_needed);
  ASSERT_EQ(fidl::coded::Type::Kind::kArray, type3->kind);
  auto type3_array = static_cast<const fidl::coded::ArrayType*>(type3);
  EXPECT_EQ(11u * 7 * 1, type3_array->element_size_v2);
  EXPECT_EQ(type2, type3_array->element_type);
}

TEST(CodedTypesGeneratorTests, GoodCodedTypesOfVectors) {
  TestLibrary library(R"FIDL(library example;

type SomeStruct = struct {};

type Vectors = struct {
    bytes1 vector<SomeStruct>:10;
    bytes12 vector<vector<SomeStruct>:10>:20;
};
)FIDL");
  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();

  auto name_some_struct = fidl::flat::Name::Key(library.LookupLibrary("example"), "SomeStruct");
  auto type_some_struct = gen.CodedTypeFor(name_some_struct);
  ASSERT_NE(type_some_struct, nullptr);
  EXPECT_EQ("example_SomeStruct", type_some_struct->coded_name);
  EXPECT_TRUE(type_some_struct->is_coding_needed);
  ASSERT_EQ(fidl::coded::Type::Kind::kStruct, type_some_struct->kind);
  auto type_some_struct_struct = static_cast<const fidl::coded::StructType*>(type_some_struct);
  ASSERT_TRUE(type_some_struct_struct->is_empty);
  ASSERT_EQ(0u, type_some_struct_struct->elements.size());
  EXPECT_EQ("example/SomeStruct", type_some_struct_struct->qname);
  EXPECT_EQ(type_some_struct_struct->maybe_reference_type, nullptr);
  EXPECT_EQ(1u, type_some_struct_struct->size_v2);

  ASSERT_EQ(2u, gen.coded_types().size());

  auto type0 = gen.coded_types().at(0).get();
  EXPECT_EQ("Vector10nonnullable18example_SomeStruct", type0->coded_name);
  EXPECT_TRUE(type0->is_coding_needed);
  ASSERT_EQ(fidl::coded::Type::Kind::kVector, type0->kind);
  auto type0_vector = static_cast<const fidl::coded::VectorType*>(type0);
  EXPECT_EQ(type_some_struct, type0_vector->element_type);
  EXPECT_EQ(10u, type0_vector->max_count);
  EXPECT_EQ(1u, type0_vector->element_size_v2);
  EXPECT_EQ(fidl::types::Nullability::kNonnullable, type0_vector->nullability);

  auto type1 = gen.coded_types().at(1).get();
  EXPECT_EQ("Vector20nonnullable39Vector10nonnullable18example_SomeStruct", type1->coded_name);
  EXPECT_TRUE(type1->is_coding_needed);
  ASSERT_EQ(fidl::coded::Type::Kind::kVector, type1->kind);
  auto type1_vector = static_cast<const fidl::coded::VectorType*>(type1);
  EXPECT_EQ(type0, type1_vector->element_type);
  EXPECT_EQ(20u, type1_vector->max_count);
  EXPECT_EQ(16u, type1_vector->element_size_v2);
  EXPECT_EQ(fidl::types::Nullability::kNonnullable, type1_vector->nullability);
}

TEST(CodedTypesGeneratorTests, GoodCodedTypesOfProtocols) {
  TestLibrary library(R"FIDL(library example;

protocol SomeProtocol {};

type OnReceivePayload = resource struct {
    server server_end:SomeProtocol;
};

protocol UseOfProtocol {
    Call(resource struct {
        client client_end:SomeProtocol;
    });
    -> OnReceive(OnReceivePayload);
};
)FIDL");
  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();

  ASSERT_EQ(2u, gen.coded_types().size());

  auto type0 = gen.coded_types().at(0).get();
  EXPECT_EQ("Protocol20example_SomeProtocolnonnullable", type0->coded_name);
  EXPECT_TRUE(type0->is_coding_needed);
  EXPECT_EQ(4u, type0->size_v2);
  ASSERT_EQ(fidl::coded::Type::Kind::kProtocolHandle, type0->kind);
  auto type0_ihandle = static_cast<const fidl::coded::ProtocolHandleType*>(type0);
  ASSERT_EQ(fidl::types::Nullability::kNonnullable, type0_ihandle->nullability);

  auto type1 = gen.coded_types().at(1).get();
  EXPECT_EQ("Request20example_SomeProtocolnonnullable", type1->coded_name);
  EXPECT_TRUE(type1->is_coding_needed);
  EXPECT_EQ(4u, type1->size_v2);
  ASSERT_EQ(fidl::coded::Type::Kind::kRequestHandle, type1->kind);
  auto type1_ihandle = static_cast<const fidl::coded::RequestHandleType*>(type1);
  ASSERT_EQ(fidl::types::Nullability::kNonnullable, type1_ihandle->nullability);

  auto anon_payload_name =
      fidl::flat::Name::Key(library.LookupLibrary("example"), "UseOfProtocolCallRequest");
  auto typed_anon_payload = gen.CodedTypeFor(anon_payload_name);
  EXPECT_EQ("example_UseOfProtocolCallRequest", typed_anon_payload->coded_name);
  EXPECT_TRUE(typed_anon_payload->is_coding_needed);
  EXPECT_EQ(4u, typed_anon_payload->size_v2);
  ASSERT_EQ(fidl::coded::Type::Kind::kStruct, typed_anon_payload->kind);
  auto anon_payload_message = static_cast<const fidl::coded::StructType*>(typed_anon_payload);
  EXPECT_EQ("example/UseOfProtocolCallRequest", anon_payload_message->qname);
  EXPECT_EQ(1u, anon_payload_message->elements.size());
  EXPECT_EQ(0u, field(anon_payload_message->elements.at(0)).offset_v2);
  EXPECT_EQ(type0, field(anon_payload_message->elements.at(0)).type);

  auto named_payload_name =
      fidl::flat::Name::Key(library.LookupLibrary("example"), "OnReceivePayload");
  auto type_named_payload = gen.CodedTypeFor(named_payload_name);
  ASSERT_NE(type_named_payload, nullptr);
  EXPECT_EQ("example_OnReceivePayload", type_named_payload->coded_name);
  EXPECT_TRUE(type_named_payload->is_coding_needed);
  EXPECT_EQ(4u, type_named_payload->size_v2);
  ASSERT_EQ(fidl::coded::Type::Kind::kStruct, type_named_payload->kind);
  auto type_named_payload_message = static_cast<const fidl::coded::StructType*>(type_named_payload);
  ASSERT_FALSE(type_named_payload_message->is_empty);
  EXPECT_EQ(type_named_payload_message->maybe_reference_type, nullptr);
  EXPECT_EQ("example/OnReceivePayload", type_named_payload_message->qname);
  ASSERT_EQ(1u, type_named_payload_message->elements.size());
  EXPECT_EQ(0u, field(type_named_payload_message->elements.at(0)).offset_v2);
  EXPECT_EQ(type1, field(type_named_payload_message->elements.at(0)).type);
}

TEST(CodedTypesGeneratorTests, GoodCodedTypesOfProtocolErrorSyntax) {
  TestLibrary library(R"FIDL(library example;

protocol SomeProtocol {};

protocol UseOfProtocol {
    strict Method() -> (resource struct {
        client client_end:SomeProtocol;
    }) error uint32;
};
)FIDL");
  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();

  ASSERT_EQ(3u, gen.coded_types().size());

  auto type0 = gen.coded_types().at(0).get();
  EXPECT_EQ("example_UseOfProtocol_Method_ResultNullableRef", type0->coded_name);
  EXPECT_TRUE(type0->is_coding_needed);
  ASSERT_EQ(fidl::coded::Type::Kind::kUnion, type0->kind);
  auto type0_union = static_cast<const fidl::coded::UnionType*>(type0);
  ASSERT_EQ(fidl::types::Nullability::kNullable, type0_union->nullability);
  EXPECT_EQ(16u, type0->size_v2);
  ASSERT_EQ(2u, type0_union->fields.size());
  auto type0_field0 = type0_union->fields.at(0);
  EXPECT_EQ("example_UseOfProtocol_Method_Response", type0_field0.type->coded_name);
  auto type0_field1 = type0_union->fields.at(1);
  EXPECT_EQ("uint32", type0_field1.type->coded_name);

  auto type2 = gen.coded_types().at(1).get();
  EXPECT_EQ("Protocol20example_SomeProtocolnonnullable", type2->coded_name);
  EXPECT_TRUE(type2->is_coding_needed);
  EXPECT_EQ(4u, type2->size_v2);
  ASSERT_EQ(fidl::coded::Type::Kind::kProtocolHandle, type2->kind);
  auto type2_ihandle = static_cast<const fidl::coded::ProtocolHandleType*>(type2);
  ASSERT_EQ(fidl::types::Nullability::kNonnullable, type2_ihandle->nullability);

  auto type3 = gen.coded_types().at(2).get();
  EXPECT_EQ("uint32", type3->coded_name);

  auto result_union_name =
      fidl::flat::Name::Key(library.LookupLibrary("example"), "UseOfProtocol_Method_Result");
  auto typed_result_union = gen.CodedTypeFor(result_union_name);
  EXPECT_EQ("example_UseOfProtocol_Method_Result", typed_result_union->coded_name);
  EXPECT_TRUE(typed_result_union->is_coding_needed);
  EXPECT_EQ(16u, typed_result_union->size_v2);
  ASSERT_EQ(fidl::coded::Type::Kind::kUnion, typed_result_union->kind);
  auto result_union_message = static_cast<const fidl::coded::UnionType*>(typed_result_union);
  EXPECT_EQ("example/UseOfProtocol_Method_Result", result_union_message->qname);
  EXPECT_EQ(2u, result_union_message->fields.size());

  auto anon_payload_name =
      fidl::flat::Name::Key(library.LookupLibrary("example"), "UseOfProtocol_Method_Response");
  auto type_anon_payload = gen.CodedTypeFor(anon_payload_name);
  ASSERT_NE(type_anon_payload, nullptr);
  EXPECT_EQ("example_UseOfProtocol_Method_Response", type_anon_payload->coded_name);
  EXPECT_TRUE(type_anon_payload->is_coding_needed);
  EXPECT_EQ(4u, type_anon_payload->size_v2);
  ASSERT_EQ(fidl::coded::Type::Kind::kStruct, type_anon_payload->kind);
  auto type_anon_payload_message = static_cast<const fidl::coded::StructType*>(type_anon_payload);
  ASSERT_FALSE(type_anon_payload_message->is_empty);
  EXPECT_EQ(type_anon_payload_message->maybe_reference_type, nullptr);
  EXPECT_EQ("example/UseOfProtocol_Method_Response", type_anon_payload_message->qname);
  ASSERT_EQ(1u, type_anon_payload_message->elements.size());
  EXPECT_EQ(0u, field(type_anon_payload_message->elements.at(0)).offset_v2);
  EXPECT_EQ(type2, field(type_anon_payload_message->elements.at(0)).type);
}

TEST(CodedTypesGeneratorTests, GoodCodedTypesGeneratedWrappers) {
  TestLibrary library(R"FIDL(library example;

protocol ErrorSyntaxProtocol {
    strict ErrorSyntaxMethod() -> () error uint32;
};
)FIDL");
  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();

  ASSERT_EQ(2u, gen.coded_types().size());

  auto type0 = gen.coded_types().at(0).get();
  EXPECT_EQ("example_ErrorSyntaxProtocol_ErrorSyntaxMethod_ResultNullableRef", type0->coded_name);
  EXPECT_EQ(16u, type0->size_v2);

  auto type1 = gen.coded_types().at(1).get();
  EXPECT_EQ("uint32", type1->coded_name);
}

TEST(CodedTypesGeneratorTests, GoodCodedTypesOfProtocolEnds) {
  TestLibrary library(R"FIDL(library example;

protocol SomeProtocol {};

protocol UseOfProtocolEnds {
    strict ClientEnds(resource struct {
        in client_end:SomeProtocol;
    }) -> (resource struct {
        out client_end:<SomeProtocol, optional>;
    });
    strict ServerEnds(resource struct {
        in server_end:<SomeProtocol, optional>;
    }) -> (resource struct {
        out server_end:SomeProtocol;
    });
};
)FIDL");
  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();

  ASSERT_EQ(4u, gen.coded_types().size());

  // ClientEnd request payload
  auto type0 = gen.coded_types().at(3).get();
  EXPECT_EQ("Protocol20example_SomeProtocolnonnullable", type0->coded_name);
  EXPECT_TRUE(type0->is_coding_needed);
  EXPECT_EQ(4u, type0->size_v2);
  ASSERT_EQ(fidl::coded::Type::Kind::kProtocolHandle, type0->kind);
  auto type0_ihandle = static_cast<const fidl::coded::ProtocolHandleType*>(type0);
  EXPECT_EQ(fidl::types::Nullability::kNonnullable, type0_ihandle->nullability);

  // ClientEnd response payload
  auto type1 = gen.coded_types().at(2).get();
  EXPECT_EQ("Protocol20example_SomeProtocolnullable", type1->coded_name);
  EXPECT_TRUE(type1->is_coding_needed);
  EXPECT_EQ(4u, type1->size_v2);
  ASSERT_EQ(fidl::coded::Type::Kind::kProtocolHandle, type1->kind);
  auto type2_ihandle = static_cast<const fidl::coded::ProtocolHandleType*>(type1);
  EXPECT_EQ(fidl::types::Nullability::kNullable, type2_ihandle->nullability);

  // ServerEnd request payload
  auto type2 = gen.coded_types().at(1).get();
  EXPECT_EQ("Request20example_SomeProtocolnullable", type2->coded_name);
  EXPECT_TRUE(type2->is_coding_needed);
  EXPECT_EQ(4u, type2->size_v2);
  ASSERT_EQ(fidl::coded::Type::Kind::kRequestHandle, type2->kind);
  auto type4_ihandle = static_cast<const fidl::coded::RequestHandleType*>(type2);
  EXPECT_EQ(fidl::types::Nullability::kNullable, type4_ihandle->nullability);

  // ServerEnd response payload
  auto type3 = gen.coded_types().at(0).get();
  EXPECT_EQ("Request20example_SomeProtocolnonnullable", type3->coded_name);
  EXPECT_TRUE(type3->is_coding_needed);
  EXPECT_EQ(4u, type3->size_v2);
  ASSERT_EQ(fidl::coded::Type::Kind::kRequestHandle, type3->kind);
  auto type6_ihandle = static_cast<const fidl::coded::RequestHandleType*>(type3);
  EXPECT_EQ(fidl::types::Nullability::kNonnullable, type6_ihandle->nullability);
}

// The code between |CodedTypesOfUnions| and |CodedTypesOfNullableUnions| is now very similar
// because the compiler emits both the non-nullable and nullable union types regardless of whether
// it is used in the library in which it was defined.
TEST(CodedTypesGeneratorTests, GoodCodedTypesOfUnions) {
  TestLibrary library(R"FIDL(library example;

type MyUnion = strict union {
    1: foo bool;
    2: bar int32;
};

type MyUnionStruct = struct {
  u MyUnion;
};

)FIDL");
  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();

  ASSERT_EQ(3u, gen.coded_types().size());

  auto type0 = gen.coded_types().at(0).get();
  ASSERT_EQ("example_MyUnionNullableRef", type0->coded_name);
  ASSERT_TRUE(type0->is_coding_needed);
  ASSERT_EQ(fidl::coded::Type::Kind::kUnion, type0->kind);
  auto nullable_union = static_cast<const fidl::coded::UnionType*>(type0);
  ASSERT_EQ(fidl::types::Nullability::kNullable, nullable_union->nullability);

  auto type1 = gen.coded_types().at(1).get();
  ASSERT_EQ("bool", type1->coded_name);
  ASSERT_TRUE(type1->is_coding_needed);
  ASSERT_EQ(fidl::coded::Type::Kind::kPrimitive, type1->kind);
  auto type2_primitive = static_cast<const fidl::coded::PrimitiveType*>(type1);
  ASSERT_EQ(fidl::types::PrimitiveSubtype::kBool, type2_primitive->subtype);

  auto type2 = gen.coded_types().at(2).get();
  ASSERT_EQ("int32", type2->coded_name);
  ASSERT_TRUE(type2->is_coding_needed);
  ASSERT_EQ(fidl::coded::Type::Kind::kPrimitive, type2->kind);
  auto type1_primitive = static_cast<const fidl::coded::PrimitiveType*>(type2);
  ASSERT_EQ(fidl::types::PrimitiveSubtype::kInt32, type1_primitive->subtype);

  auto name = fidl::flat::Name::Key(library.LookupLibrary("example"), "MyUnion");
  auto type = gen.CodedTypeFor(name);
  ASSERT_NE(type, nullptr);
  ASSERT_EQ("example_MyUnion", type->coded_name);
  ASSERT_TRUE(type->is_coding_needed);
  ASSERT_EQ(fidl::coded::Type::Kind::kUnion, type->kind);
  auto coded_union = static_cast<const fidl::coded::UnionType*>(type);
  ASSERT_EQ(2u, coded_union->fields.size());
  auto union_field0 = coded_union->fields.at(0);
  ASSERT_EQ(fidl::coded::Type::Kind::kPrimitive, union_field0.type->kind);
  auto union_field0_primitive = static_cast<const fidl::coded::PrimitiveType*>(union_field0.type);
  ASSERT_EQ(fidl::types::PrimitiveSubtype::kBool, union_field0_primitive->subtype);
  auto union_field1 = coded_union->fields.at(1);
  ASSERT_EQ(fidl::coded::Type::Kind::kPrimitive, union_field1.type->kind);
  auto union_field1_primitive = static_cast<const fidl::coded::PrimitiveType*>(union_field1.type);
  ASSERT_EQ(fidl::types::PrimitiveSubtype::kInt32, union_field1_primitive->subtype);
  ASSERT_EQ("example/MyUnion", coded_union->qname);
  ASSERT_EQ(fidl::types::Nullability::kNonnullable, coded_union->nullability);
  ASSERT_NE(coded_union->maybe_reference_type, nullptr);

  auto struct_name = fidl::flat::Name::Key(library.LookupLibrary("example"), "MyUnionStruct");
  auto struct_type = gen.CodedTypeFor(struct_name);
  ASSERT_NE(struct_type, nullptr);
  ASSERT_EQ("example_MyUnionStruct", struct_type->coded_name);
  ASSERT_TRUE(struct_type->is_coding_needed);
  ASSERT_EQ(fidl::coded::Type::Kind::kStruct, struct_type->kind);
  auto struct_type_struct = static_cast<const fidl::coded::StructType*>(struct_type);
  ASSERT_FALSE(struct_type_struct->is_empty);
}

// The code between |CodedTypesOfUnions| and |CodedTypesOfNullableUnions| is now very similar
// because the compiler emits both the non-nullable and nullable union types regardless of whether
// it is used in the library in which it was defined.
TEST(CodedTypesGeneratorTests, GoodCodedTypesOfNullableUnions) {
  TestLibrary library(R"FIDL(library example;

type MyUnion = strict union {
    1: foo bool;
    2: bar int32;
};

type Wrapper1 = struct {
    xu MyUnion:optional;
};

// This ensures that MyUnion? doesn't show up twice in the coded types.
type Wrapper2 = struct {
    xu MyUnion:optional;
};
)FIDL");
  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();

  // 3 == size of {bool, int32, MyUnion?}, which is all of the types used in
  // the example.
  ASSERT_EQ(3u, gen.coded_types().size());

  auto type0 = gen.coded_types().at(0).get();
  ASSERT_EQ("example_MyUnionNullableRef", type0->coded_name);
  ASSERT_TRUE(type0->is_coding_needed);
  ASSERT_EQ(fidl::coded::Type::Kind::kUnion, type0->kind);
  auto nullable_union = static_cast<const fidl::coded::UnionType*>(type0);
  ASSERT_EQ(fidl::types::Nullability::kNullable, nullable_union->nullability);

  auto type1 = gen.coded_types().at(1).get();
  ASSERT_EQ("bool", type1->coded_name);
  ASSERT_TRUE(type1->is_coding_needed);
  ASSERT_EQ(fidl::coded::Type::Kind::kPrimitive, type1->kind);
  auto type2_primitive = static_cast<const fidl::coded::PrimitiveType*>(type1);
  ASSERT_EQ(fidl::types::PrimitiveSubtype::kBool, type2_primitive->subtype);

  auto type2 = gen.coded_types().at(2).get();
  ASSERT_EQ("int32", type2->coded_name);
  ASSERT_TRUE(type2->is_coding_needed);
  ASSERT_EQ(fidl::coded::Type::Kind::kPrimitive, type2->kind);
  auto type1_primitive = static_cast<const fidl::coded::PrimitiveType*>(type2);
  ASSERT_EQ(fidl::types::PrimitiveSubtype::kInt32, type1_primitive->subtype);
}

// This mostly exists to make sure that the same nullable objects aren't
// represented more than once in the coding tables.
TEST(CodedTypesGeneratorTests, GoodCodedTypesOfNullablePointers) {
  TestLibrary library(R"FIDL(library example;

type MyStruct = struct {
    foo bool;
    bar int32;
};

type MyUnion = strict union {
    1: foo bool;
    2: bar int32;
};

type Wrapper1 = struct {
    ms box<MyStruct>;
    mu MyUnion:optional;
    xu MyUnion:optional;
};

// This ensures that MyUnion? doesn't show up twice in the coded types.
type Wrapper2 = struct {
    ms box<MyStruct>;
    mu MyUnion:optional;
    xu MyUnion:optional;
};
)FIDL");
  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();

  // 4 == size of {bool, int32, MyStruct?, MyUnion?},
  // which are all the coded types in the example.
  ASSERT_EQ(4u, gen.coded_types().size());
}

TEST(CodedTypesGeneratorTests, GoodCodedHandle) {
  TestLibrary library(R"FIDL(library example;

type ObjType = strict enum : uint32 {
    NONE = 0;
    VMO = 3;
};

type Rights = strict bits {
    SOME_RIGHT = 1;
};

resource_definition handle : uint32 {
    properties {
        subtype ObjType;
        rights Rights;
    };
};

type MyStruct = resource struct {
    h handle:<VMO, Rights.SOME_RIGHT>;
};
)FIDL");

  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();

  auto struct_name = fidl::flat::Name::Key(library.LookupLibrary("example"), "MyStruct");
  auto struct_type = static_cast<const fidl::coded::StructType*>(gen.CodedTypeFor(struct_name));
  auto handle_type =
      static_cast<const fidl::coded::HandleType*>(field(struct_type->elements[0]).type);

  ASSERT_EQ(fidl::types::HandleSubtype::kVmo, handle_type->subtype);
  ASSERT_EQ(1u, handle_type->rights);
  ASSERT_EQ(fidl::types::Nullability::kNonnullable, handle_type->nullability);
}

TEST(CodedTypesGeneratorTests, GoodCodedTypesOfStructsWithPaddings) {
  TestLibrary library(R"FIDL(library example;

type BoolAndInt32 = struct {
    foo bool;
    // 3 bytes of padding here.
    bar int32;
};

type Complex = struct {
    i32 int32;
    b1 bool;
    // 3 bytes of padding here.
    i64 int64;
    i16 int16;
// 6 bytes of padding here.
};
)FIDL");
  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();

  ASSERT_EQ(4u, gen.coded_types().size());

  auto type0 = gen.coded_types().at(0).get();
  EXPECT_EQ("int32", type0->coded_name);
  EXPECT_TRUE(type0->is_coding_needed);
  auto type1 = gen.coded_types().at(1).get();
  EXPECT_EQ("bool", type1->coded_name);
  EXPECT_TRUE(type1->is_coding_needed);
  auto type2 = gen.coded_types().at(2).get();
  EXPECT_EQ("int64", type2->coded_name);
  EXPECT_TRUE(type2->is_coding_needed);
  auto type3 = gen.coded_types().at(3).get();
  EXPECT_EQ("int16", type3->coded_name);
  EXPECT_TRUE(type3->is_coding_needed);

  auto name_bool_and_int32 =
      fidl::flat::Name::Key(library.LookupLibrary("example"), "BoolAndInt32");
  auto type_bool_and_int32 = gen.CodedTypeFor(name_bool_and_int32);
  ASSERT_NE(type_bool_and_int32, nullptr);
  EXPECT_EQ("example_BoolAndInt32", type_bool_and_int32->coded_name);
  auto type_bool_and_int32_struct =
      static_cast<const fidl::coded::StructType*>(type_bool_and_int32);
  ASSERT_FALSE(type_bool_and_int32_struct->is_empty);
  ASSERT_EQ(type_bool_and_int32_struct->elements.size(), 2u);
  EXPECT_EQ(field(type_bool_and_int32_struct->elements[0]).type->kind,
            fidl::coded::Type::Kind::kPrimitive);
  EXPECT_EQ(field(type_bool_and_int32_struct->elements[0]).offset_v2, 0u);
  EXPECT_EQ(padding(type_bool_and_int32_struct->elements[1]).offset_v2, 0u);
  EXPECT_EQ(std::get<uint32_t>(padding(type_bool_and_int32_struct->elements[1]).mask), 0xffffff00);

  auto name_complex = fidl::flat::Name::Key(library.LookupLibrary("example"), "Complex");
  auto type_complex = gen.CodedTypeFor(name_complex);
  ASSERT_NE(type_complex, nullptr);
  EXPECT_EQ("example_Complex", type_complex->coded_name);
  auto type_complex_struct = static_cast<const fidl::coded::StructType*>(type_complex);
  ASSERT_FALSE(type_complex_struct->is_empty);
  ASSERT_EQ(type_complex_struct->elements.size(), 3u);
  EXPECT_EQ(field(type_complex_struct->elements[0]).type->kind,
            fidl::coded::Type::Kind::kPrimitive);
  EXPECT_EQ(field(type_complex_struct->elements[0]).offset_v2, 4u);
  EXPECT_EQ(padding(type_complex_struct->elements[1]).offset_v2, 4u);
  EXPECT_EQ(std::get<uint32_t>(padding(type_complex_struct->elements[1]).mask), 0xffffff00);
  EXPECT_EQ(padding(type_complex_struct->elements[2]).offset_v2, 16u);
  EXPECT_EQ(std::get<uint64_t>(padding(type_complex_struct->elements[2]).mask),
            0xffffffffffff0000ull);
}

TEST(CodedTypesGeneratorTests, GoodCodedTypesOfMultilevelNestedStructs) {
  TestLibrary library(R"FIDL(library example;

// alignment 4
type Level0 = struct {
    a int8;
    //padding 3
    b int32;
    c int8;
// padding 3;
};

// alignment 8
type Level1 = struct {
    l0 Level0;
    // 4 bytes padding + 3 inside of Level0.
    d uint64;
};

// alignment 8
type Level2 = struct {
    l1 Level1;
    e uint8;
// 7 bytes of padding.
};
)FIDL");
  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();

  auto name_level0 = fidl::flat::Name::Key(library.LookupLibrary("example"), "Level0");
  auto type_level0 = gen.CodedTypeFor(name_level0);
  ASSERT_NE(type_level0, nullptr);
  auto struct_level0 = static_cast<const fidl::coded::StructType*>(type_level0);
  ASSERT_FALSE(struct_level0->is_empty);
  ASSERT_EQ(struct_level0->elements.size(), 2u);
  EXPECT_EQ(padding(struct_level0->elements[0]).offset_v2, 0u);
  EXPECT_EQ(std::get<uint32_t>(padding(struct_level0->elements[0]).mask), 0xffffff00);
  EXPECT_EQ(padding(struct_level0->elements[1]).offset_v2, 8u);
  EXPECT_EQ(std::get<uint32_t>(padding(struct_level0->elements[1]).mask), 0xffffff00);

  auto name_level1 = fidl::flat::Name::Key(library.LookupLibrary("example"), "Level1");
  auto type_level1 = gen.CodedTypeFor(name_level1);
  ASSERT_NE(type_level1, nullptr);
  auto struct_level1 = static_cast<const fidl::coded::StructType*>(type_level1);
  ASSERT_FALSE(struct_level1->is_empty);
  ASSERT_EQ(struct_level1->elements.size(), 2u);
  EXPECT_EQ(padding(struct_level1->elements[0]).offset_v2, 0u);
  EXPECT_EQ(std::get<uint32_t>(padding(struct_level1->elements[0]).mask), 0xffffff00);
  EXPECT_EQ(padding(struct_level1->elements[1]).offset_v2, 8u);
  EXPECT_EQ(std::get<uint64_t>(padding(struct_level1->elements[1]).mask), 0xffffffffffffff00);

  auto name_level2 = fidl::flat::Name::Key(library.LookupLibrary("example"), "Level2");
  auto type_level2 = gen.CodedTypeFor(name_level2);
  ASSERT_NE(type_level2, nullptr);
  auto struct_level2 = static_cast<const fidl::coded::StructType*>(type_level2);
  ASSERT_FALSE(struct_level2->is_empty);
  ASSERT_EQ(struct_level2->elements.size(), 3u);
  EXPECT_EQ(padding(struct_level2->elements[0]).offset_v2, 0u);
  EXPECT_EQ(std::get<uint32_t>(padding(struct_level2->elements[0]).mask), 0xffffff00);
  EXPECT_EQ(padding(struct_level2->elements[1]).offset_v2, 8u);
  EXPECT_EQ(std::get<uint64_t>(padding(struct_level2->elements[1]).mask), 0xffffffffffffff00);
  EXPECT_EQ(padding(struct_level2->elements[2]).offset_v2, 24u);
  EXPECT_EQ(std::get<uint64_t>(padding(struct_level2->elements[2]).mask), 0xffffffffffffff00);
}

TEST(CodedTypesGeneratorTests, GoodCodedTypesOfRecursiveOptionalStructs) {
  TestLibrary library(R"FIDL(library example;

type OneLevelRecursiveOptionalStruct = struct {
    val box<OneLevelRecursiveOptionalStruct>;
};

type TwoLevelRecursiveOptionalStructA = struct {
    b TwoLevelRecursiveOptionalStructB;
};

type TwoLevelRecursiveOptionalStructB = struct {
    a box<TwoLevelRecursiveOptionalStructA>;
};
)FIDL");
  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();

  auto name_one_level =
      fidl::flat::Name::Key(library.LookupLibrary("example"), "OneLevelRecursiveOptionalStruct");
  auto type_one_level = gen.CodedTypeFor(name_one_level);
  ASSERT_NE(type_one_level, nullptr);
  auto struct_one_level = static_cast<const fidl::coded::StructType*>(type_one_level);
  ASSERT_FALSE(struct_one_level->is_empty);
  ASSERT_EQ(struct_one_level->elements.size(), 1u);
  EXPECT_EQ(field(struct_one_level->elements[0]).type->kind,
            fidl::coded::Type::Kind::kStructPointer);
  ASSERT_THAT(field(struct_one_level->elements[0]).type->coded_name,
              HasSubstr("OneLevelRecursiveOptionalStruct"));
  EXPECT_EQ(field(struct_one_level->elements[0]).offset_v2, 0u);

  auto name_two_level_b =
      fidl::flat::Name::Key(library.LookupLibrary("example"), "TwoLevelRecursiveOptionalStructB");
  auto type_two_level_b = gen.CodedTypeFor(name_two_level_b);
  ASSERT_NE(type_two_level_b, nullptr);
  auto struct_two_level_b = static_cast<const fidl::coded::StructType*>(type_two_level_b);
  ASSERT_FALSE(struct_two_level_b->is_empty);
  ASSERT_EQ(struct_two_level_b->elements.size(), 1u);
  EXPECT_EQ(field(struct_two_level_b->elements[0]).type->kind,
            fidl::coded::Type::Kind::kStructPointer);
  ASSERT_THAT(field(struct_two_level_b->elements[0]).type->coded_name,
              HasSubstr("TwoLevelRecursiveOptionalStructA"));
  EXPECT_EQ(field(struct_two_level_b->elements[0]).offset_v2, 0u);

  // TwoLevelRecursiveOptionalStructA will be equivalent to TwoLevelRecursiveOptionalStructB
  // because of flattening.
  auto name_two_level_a =
      fidl::flat::Name::Key(library.LookupLibrary("example"), "TwoLevelRecursiveOptionalStructA");
  auto type_two_level_a = gen.CodedTypeFor(name_two_level_a);
  ASSERT_NE(type_two_level_a, nullptr);
  auto struct_two_level_a = static_cast<const fidl::coded::StructType*>(type_two_level_a);
  ASSERT_FALSE(struct_two_level_a->is_empty);
  ASSERT_EQ(struct_two_level_a->elements.size(), 1u);
  EXPECT_EQ(field(struct_two_level_a->elements[0]).type->kind,
            fidl::coded::Type::Kind::kStructPointer);
  ASSERT_THAT(field(struct_two_level_a->elements[0]).type->coded_name,
              HasSubstr("TwoLevelRecursiveOptionalStructA"));
  EXPECT_EQ(field(struct_two_level_a->elements[0]).offset_v2, 0u);
}

TEST(CodedTypesGeneratorTests, GoodCodedTypesOfReusedStructs) {
  TestLibrary library(R"FIDL(library example;

// InnerStruct is reused and appears twice.
type InnerStruct = struct{
    a int8;
    // 1 byte padding
    b int16;
};

type OuterStruct = struct {
    a InnerStruct;
    b InnerStruct;
};
)FIDL");
  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();

  auto name_inner_struct = fidl::flat::Name::Key(library.LookupLibrary("example"), "InnerStruct");
  auto type_inner_struct = gen.CodedTypeFor(name_inner_struct);
  ASSERT_NE(type_inner_struct, nullptr);
  auto struct_inner_struct = static_cast<const fidl::coded::StructType*>(type_inner_struct);
  ASSERT_FALSE(struct_inner_struct->is_empty);
  ASSERT_EQ(struct_inner_struct->elements.size(), 1u);
  EXPECT_EQ(padding(struct_inner_struct->elements[0]).offset_v2, 0u);
  ASSERT_TRUE(std::get<uint16_t>(padding(struct_inner_struct->elements[0]).mask));
  EXPECT_EQ(std::get<uint16_t>(padding(struct_inner_struct->elements[0]).mask), 0xff00);

  auto name_outer_struct = fidl::flat::Name::Key(library.LookupLibrary("example"), "OuterStruct");
  auto type_outer_struct = gen.CodedTypeFor(name_outer_struct);
  ASSERT_NE(type_outer_struct, nullptr);
  auto struct_outer_struct = static_cast<const fidl::coded::StructType*>(type_outer_struct);
  ASSERT_FALSE(struct_outer_struct->is_empty);
  ASSERT_EQ(struct_outer_struct->elements.size(), 2u);
  EXPECT_EQ(padding(struct_outer_struct->elements[0]).offset_v2, 0u);
  ASSERT_TRUE(std::get<uint16_t>(padding(struct_outer_struct->elements[0]).mask));
  EXPECT_EQ(std::get<uint16_t>(padding(struct_outer_struct->elements[0]).mask), 0xff00);
  EXPECT_EQ(padding(struct_outer_struct->elements[1]).offset_v2, 4u);
  ASSERT_TRUE(std::get<uint16_t>(padding(struct_outer_struct->elements[1]).mask));
  EXPECT_EQ(std::get<uint16_t>(padding(struct_outer_struct->elements[1]).mask), 0xff00);
}

TEST(CodedTypesGeneratorTests, GoodCodedTypesOfOptionals) {
  TestLibrary library(R"FIDL(
library example;
using zx;

type InnerStruct = struct {
  a int8;
  // 1 byte padding
  b int16;
};

type SimpleUnion = union {
    1: a int64;
};

type OuterStruct = resource struct {
  a InnerStruct;
  opt_handle zx.Handle:optional;
  opt_union SimpleUnion:optional;
  b InnerStruct;
};

)FIDL");
  library.UseLibraryZx();
  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();

  auto name_outer_struct = fidl::flat::Name::Key(library.LookupLibrary("example"), "OuterStruct");
  auto type_outer_struct = gen.CodedTypeFor(name_outer_struct);
  ASSERT_NE(type_outer_struct, nullptr);
  auto struct_outer_struct = static_cast<const fidl::coded::StructType*>(type_outer_struct);
  ASSERT_FALSE(struct_outer_struct->is_empty);
  ASSERT_EQ(struct_outer_struct->elements.size(), 5u);
  EXPECT_EQ(padding(struct_outer_struct->elements[0]).offset_v2, 0u);
  EXPECT_EQ(std::get<uint16_t>(padding(struct_outer_struct->elements[0]).mask), 0xff00);
  EXPECT_EQ(field(struct_outer_struct->elements[1]).type->kind, fidl::coded::Type::Kind::kHandle);
  EXPECT_EQ(field(struct_outer_struct->elements[1]).offset_v2, 4u);
  EXPECT_EQ(field(struct_outer_struct->elements[2]).type->kind, fidl::coded::Type::Kind::kUnion);
  EXPECT_EQ(field(struct_outer_struct->elements[2]).offset_v2, 8u);
  EXPECT_EQ(padding(struct_outer_struct->elements[3]).offset_v2, 24u);
  EXPECT_EQ(std::get<uint16_t>(padding(struct_outer_struct->elements[3]).mask), 0xff00);
  EXPECT_EQ(padding(struct_outer_struct->elements[4]).offset_v2, 28u);
  EXPECT_EQ(std::get<uint32_t>(padding(struct_outer_struct->elements[4]).mask), 0xffffffff);
}

// In the following example, we shadow the builtin `byte` alias to a struct.
// fidlc previous had a scoping bug where the `f1` field's `byte` type referred
// to the builtin rather than the struct. This has since been fixed. Here we
// test that the coding tables take the same interpretation, i.e. that they do
// not do their own lookups with different scoping rules.
TEST(CodedTypesGeneratorTests, GoodCodingTablesMatchScoping) {
  TestLibrary library(R"FIDL(library example;

alias membertype = uint32;

type byte = struct {
    @allow_deprecated_struct_defaults
    member membertype = 1;
};

type container = struct {
    f1 byte;
    f2 vector<uint8>;
};
)FIDL");
  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();

  auto the_struct_name = fidl::flat::Name::Key(library.LookupLibrary("example"), "container");
  auto the_coded_type = gen.CodedTypeFor(the_struct_name);
  ASSERT_NE(the_coded_type, nullptr);
  auto the_struct_coded_type = static_cast<const fidl::coded::StructType*>(the_coded_type);
  ASSERT_FALSE(the_struct_coded_type->is_empty);
  ASSERT_EQ(the_struct_coded_type->elements.size(), 2u);
  EXPECT_EQ(0xffffffff, std::get<uint32_t>(padding(the_struct_coded_type->elements[0]).mask));
  EXPECT_EQ(fidl::coded::Type::Kind::kVector, field(the_struct_coded_type->elements[1]).type->kind);
}

TEST(CodedTypesGeneratorTests, GoodCodedTypesOfTables) {
  TestLibrary library(R"FIDL(library example;

type MyTable = table {
    1: foo bool;
    2: bar int32;
    3: baz array<bool, 42>;
};
)FIDL");
  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();

  ASSERT_EQ(3u, gen.coded_types().size());

  // This bool is used in the coding table of the MyTable table.
  auto type0 = gen.coded_types().at(0).get();
  EXPECT_EQ("bool", type0->coded_name);
  EXPECT_TRUE(type0->is_coding_needed);
  ASSERT_EQ(fidl::coded::Type::Kind::kPrimitive, type0->kind);
  auto type0_primitive = static_cast<const fidl::coded::PrimitiveType*>(type0);
  EXPECT_EQ(fidl::types::PrimitiveSubtype::kBool, type0_primitive->subtype);

  auto type1 = gen.coded_types().at(1).get();
  EXPECT_EQ("int32", type1->coded_name);
  EXPECT_TRUE(type1->is_coding_needed);
  ASSERT_EQ(fidl::coded::Type::Kind::kPrimitive, type1->kind);
  auto type1_primitive = static_cast<const fidl::coded::PrimitiveType*>(type1);
  EXPECT_EQ(fidl::types::PrimitiveSubtype::kInt32, type1_primitive->subtype);

  auto type3 = gen.coded_types().at(2).get();
  EXPECT_EQ("Array42_4bool", type3->coded_name);
  EXPECT_TRUE(type3->is_coding_needed);
  ASSERT_EQ(fidl::coded::Type::Kind::kArray, type3->kind);
  auto type3_array = static_cast<const fidl::coded::ArrayType*>(type3);
  EXPECT_EQ(42u, type3_array->size_v2);
  ASSERT_EQ(fidl::coded::Type::Kind::kPrimitive, type3_array->element_type->kind);
  auto type3_array_element_type =
      static_cast<const fidl::coded::PrimitiveType*>(type3_array->element_type);
  EXPECT_EQ(fidl::types::PrimitiveSubtype::kBool, type3_array_element_type->subtype);

  auto name_table = fidl::flat::Name::Key(library.LookupLibrary("example"), "MyTable");
  auto type_table = gen.CodedTypeFor(name_table);
  ASSERT_NE(type_table, nullptr);
  EXPECT_EQ("example_MyTable", type_table->coded_name);
  EXPECT_TRUE(type_table->is_coding_needed);
  ASSERT_EQ(fidl::coded::Type::Kind::kTable, type_table->kind);
  auto type_table_table = static_cast<const fidl::coded::TableType*>(type_table);
  EXPECT_EQ(3u, type_table_table->fields.size());
  auto table_field0 = type_table_table->fields.at(0);
  ASSERT_EQ(fidl::coded::Type::Kind::kPrimitive, table_field0.type->kind);
  auto table_field0_primitive = static_cast<const fidl::coded::PrimitiveType*>(table_field0.type);
  ASSERT_EQ(fidl::types::PrimitiveSubtype::kBool, table_field0_primitive->subtype);
  auto table_field1 = type_table_table->fields.at(1);
  ASSERT_EQ(fidl::coded::Type::Kind::kPrimitive, table_field1.type->kind);
  auto table_field1_primitive = static_cast<const fidl::coded::PrimitiveType*>(table_field1.type);
  ASSERT_EQ(fidl::types::PrimitiveSubtype::kInt32, table_field1_primitive->subtype);
  auto table_field2 = type_table_table->fields.at(2);
  ASSERT_EQ(fidl::coded::Type::Kind::kArray, table_field2.type->kind);
  EXPECT_EQ("example/MyTable", type_table_table->qname);
}

TEST(CodedTypesGeneratorTests, GoodCodedTypesOfBits) {
  TestLibrary library(R"FIDL(library example;

type StrictBits = strict bits : uint8 {
    HELLO = 0x1;
    WORLD = 0x10;
};

type FlexibleBits = flexible bits : uint8 {
    HELLO = 0x1;
    WORLD = 0x10;
};
)FIDL");
  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();

  ASSERT_EQ(0u, gen.coded_types().size());
  {
    auto name_bits = fidl::flat::Name::Key(library.LookupLibrary("example"), "StrictBits");
    auto type_bits = gen.CodedTypeFor(name_bits);
    ASSERT_NE(type_bits, nullptr);
    EXPECT_EQ("example_StrictBits", type_bits->coded_name);
    EXPECT_TRUE(type_bits->is_coding_needed);
    ASSERT_EQ(fidl::coded::Type::Kind::kBits, type_bits->kind);
    auto type_bits_bits = static_cast<const fidl::coded::BitsType*>(type_bits);
    EXPECT_EQ(fidl::types::PrimitiveSubtype::kUint8, type_bits_bits->subtype);
    EXPECT_EQ(fidl::types::Strictness::kStrict, type_bits_bits->strictness);
    EXPECT_EQ(0x1u | 0x10u, type_bits_bits->mask);
  }
  {
    auto name_bits = fidl::flat::Name::Key(library.LookupLibrary("example"), "FlexibleBits");
    auto type_bits = gen.CodedTypeFor(name_bits);
    ASSERT_NE(type_bits, nullptr);
    EXPECT_EQ("example_FlexibleBits", type_bits->coded_name);
    EXPECT_TRUE(type_bits->is_coding_needed);
    ASSERT_EQ(fidl::coded::Type::Kind::kBits, type_bits->kind);
    auto type_bits_bits = static_cast<const fidl::coded::BitsType*>(type_bits);
    EXPECT_EQ(fidl::types::PrimitiveSubtype::kUint8, type_bits_bits->subtype);
    EXPECT_EQ(fidl::types::Strictness::kFlexible, type_bits_bits->strictness);
    EXPECT_EQ(0x1u | 0x10u, type_bits_bits->mask);
  }
}

TEST(CodedTypesGeneratorTests, GoodCodedTypesOfStrictEnum) {
  TestLibrary library(R"FIDL(library example;

type StrictEnum = strict enum : uint16 {
    HELLO = 0x1;
    WORLD = 0x10;
};

type FlexibleEnum = flexible enum : uint16 {
    HELLO = 0x1;
    WORLD = 0x10;
};
)FIDL");
  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();

  ASSERT_EQ(0u, gen.coded_types().size());
  {
    auto name_enum = fidl::flat::Name::Key(library.LookupLibrary("example"), "StrictEnum");
    auto type_enum = gen.CodedTypeFor(name_enum);
    ASSERT_NE(type_enum, nullptr);
    EXPECT_EQ("example_StrictEnum", type_enum->coded_name);
    EXPECT_TRUE(type_enum->is_coding_needed);

    ASSERT_EQ(fidl::coded::Type::Kind::kEnum, type_enum->kind);
    auto type_enum_enum = static_cast<const fidl::coded::EnumType*>(type_enum);
    EXPECT_EQ(fidl::types::PrimitiveSubtype::kUint16, type_enum_enum->subtype);
    EXPECT_EQ(fidl::types::Strictness::kStrict, type_enum_enum->strictness);
    EXPECT_EQ(2u, type_enum_enum->members.size());
    EXPECT_EQ(0x1u, type_enum_enum->members[0]);
    EXPECT_EQ(0x10u, type_enum_enum->members[1]);
  }
  {
    auto name_enum = fidl::flat::Name::Key(library.LookupLibrary("example"), "FlexibleEnum");
    auto type_enum = gen.CodedTypeFor(name_enum);
    ASSERT_NE(type_enum, nullptr);
    EXPECT_EQ("example_FlexibleEnum", type_enum->coded_name);
    EXPECT_TRUE(type_enum->is_coding_needed);

    ASSERT_EQ(fidl::coded::Type::Kind::kEnum, type_enum->kind);
    auto type_enum_enum = static_cast<const fidl::coded::EnumType*>(type_enum);
    EXPECT_EQ(fidl::types::PrimitiveSubtype::kUint16, type_enum_enum->subtype);
    EXPECT_EQ(fidl::types::Strictness::kFlexible, type_enum_enum->strictness);
  }
}

TEST(CodedTypesGeneratorTests, GoodCodedTypesOfUnionsWithReverseOrdinals) {
  TestLibrary library(R"FIDL(library example;

type First = struct {};
type Second = struct {};

type MyUnion = strict union {
    3: second Second;
    2: reserved;
    1: first First;
};
)FIDL");
  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();

  auto name = fidl::flat::Name::Key(library.LookupLibrary("example"), "MyUnion");
  auto type = gen.CodedTypeFor(name);
  ASSERT_NE(type, nullptr);
  EXPECT_EQ("example_MyUnion", type->coded_name);
  EXPECT_TRUE(type->is_coding_needed);
  ASSERT_EQ(fidl::coded::Type::Kind::kUnion, type->kind);

  auto coded_union = static_cast<const fidl::coded::UnionType*>(type);
  ASSERT_EQ(3u, coded_union->fields.size());

  auto union_field0 = coded_union->fields.at(0);
  ASSERT_NE(union_field0.type, nullptr);
  auto union_field0_struct = static_cast<const fidl::coded::StructType*>(union_field0.type);
  ASSERT_TRUE(union_field0_struct->is_empty);
  EXPECT_EQ("example/First", union_field0_struct->qname);

  auto union_field1 = coded_union->fields.at(1);
  ASSERT_EQ(union_field1.type, nullptr);

  auto union_field2 = coded_union->fields.at(2);
  ASSERT_NE(union_field2.type, nullptr);
  auto union_field2_struct = static_cast<const fidl::coded::StructType*>(union_field2.type);
  ASSERT_TRUE(union_field2_struct->is_empty);
  EXPECT_EQ("example/Second", union_field2_struct->qname);
}

void check_duplicate_coded_type_names(const fidl::CodedTypesGenerator& gen) {
  const auto types = gen.AllCodedTypes();
  for (auto const& type : types) {
    auto count = std::count_if(types.begin(), types.end(),
                               [&](auto& t) { return t->coded_name == type->coded_name; });
    ASSERT_EQ(count, 1) << "Duplicate coded type name.";
  }
}

TEST(CodedTypesGeneratorTests, GoodDuplicateCodedTypesTwoUnions) {
  TestLibrary library(R"FIDL(library example;

type U1 = strict union {
    1: hs array<string, 2>;
};

type U2 = strict union {
    1: hss array<array<string, 2>, 2>;
};
)FIDL");
  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();
  check_duplicate_coded_type_names(gen);
}

TEST(CodedTypesGeneratorTests, GoodDuplicateCodedTypesUnionArrayArray) {
  TestLibrary library(R"FIDL(library example;

type Union = strict union {
    1: hs array<string, 2>;
    2: hss array<array<string, 2>, 2>;
};
)FIDL");
  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();
  check_duplicate_coded_type_names(gen);
}

TEST(CodedTypesGeneratorTests, GoodDuplicateCodedTypesUnionVectorArray) {
  TestLibrary library(R"FIDL(library example;

type Union = strict union {
    1: hs array<string, 2>;
    2: hss vector<array<string, 2>>:2;
};
)FIDL");
  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();
  check_duplicate_coded_type_names(gen);
}

TEST(CodedTypesGeneratorTests, GoodDuplicateCodedTypesTableArrayArray) {
  TestLibrary library(R"FIDL(library example;

type Table = table {
    1: hs array<string, 2>;
    2: hss array<array<string, 2>, 2>;
};
)FIDL");
  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();
  check_duplicate_coded_type_names(gen);
}

TEST(CodedTypesGeneratorTests, GoodUnionResourceness) {
  TestLibrary library(R"FIDL(library example;

type ResourceUnion = strict resource union {
    1: first bool;
};

type NonResourceUnion = strict union {
    1: first bool;
};
)FIDL");
  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();

  {
    auto name = fidl::flat::Name::Key(library.LookupLibrary("example"), "ResourceUnion");
    auto type = gen.CodedTypeFor(name);
    ASSERT_NE(type, nullptr);
    ASSERT_EQ(fidl::coded::Type::Kind::kUnion, type->kind);

    auto coded_union = static_cast<const fidl::coded::UnionType*>(type);
    EXPECT_EQ(fidl::types::Resourceness::kResource, coded_union->resourceness);
  }

  {
    auto name = fidl::flat::Name::Key(library.LookupLibrary("example"), "NonResourceUnion");
    auto type = gen.CodedTypeFor(name);
    ASSERT_NE(type, nullptr);
    ASSERT_EQ(fidl::coded::Type::Kind::kUnion, type->kind);

    auto coded_union = static_cast<const fidl::coded::UnionType*>(type);
    EXPECT_EQ(fidl::types::Resourceness::kValue, coded_union->resourceness);
  }
}

TEST(CodedTypesGeneratorTests, GoodTableResourceness) {
  TestLibrary library(R"FIDL(library example;

type ResourceTable = resource table {
    1: first bool;
};

type NonResourceTable = table {
    1: first bool;
};
)FIDL");
  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();

  {
    auto name = fidl::flat::Name::Key(library.LookupLibrary("example"), "ResourceTable");
    auto type = gen.CodedTypeFor(name);
    ASSERT_NE(type, nullptr);
    ASSERT_EQ(fidl::coded::Type::Kind::kTable, type->kind);

    auto coded_table = static_cast<const fidl::coded::TableType*>(type);
    EXPECT_EQ(fidl::types::Resourceness::kResource, coded_table->resourceness);
  }

  {
    auto name = fidl::flat::Name::Key(library.LookupLibrary("example"), "NonResourceTable");
    auto type = gen.CodedTypeFor(name);
    ASSERT_NE(type, nullptr);
    ASSERT_EQ(fidl::coded::Type::Kind::kTable, type->kind);

    auto coded_table = static_cast<const fidl::coded::TableType*>(type);
    EXPECT_EQ(fidl::types::Resourceness::kValue, coded_table->resourceness);
  }
}

TEST(CodedTypesGeneratorTests, GoodCodedTypesStructMessage) {
  TestLibrary library(R"FIDL(library example;

type OnReceivePayload = struct {
    arg bool;
};

protocol UseOfProtocol {
    Call(struct {
        arg1 bool;
        arg2 bool;
    });
    -> OnReceive(OnReceivePayload);
};
)FIDL");
  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();

  ASSERT_EQ(1u, gen.coded_types().size());

  auto type0 = gen.coded_types().at(0).get();
  EXPECT_EQ("bool", type0->coded_name);

  auto anon_payload_name =
      fidl::flat::Name::Key(library.LookupLibrary("example"), "UseOfProtocolCallRequest");
  auto typed_anon_payload = gen.CodedTypeFor(anon_payload_name);
  ASSERT_NE(typed_anon_payload, nullptr);
  EXPECT_EQ("example_UseOfProtocolCallRequest", typed_anon_payload->coded_name);
  EXPECT_TRUE(typed_anon_payload->is_coding_needed);
  EXPECT_EQ(2u, typed_anon_payload->size_v2);
  ASSERT_EQ(fidl::coded::Type::Kind::kStruct, typed_anon_payload->kind);
  auto anon_payload_message = static_cast<const fidl::coded::StructType*>(typed_anon_payload);
  ASSERT_FALSE(anon_payload_message->is_empty);
  EXPECT_EQ(anon_payload_message->maybe_reference_type, nullptr);
  EXPECT_EQ("example/UseOfProtocolCallRequest", anon_payload_message->qname);
  ASSERT_EQ(2u, anon_payload_message->elements.size());
  EXPECT_EQ(0u, field(anon_payload_message->elements.at(0)).offset_v2);
  EXPECT_EQ(1u, field(anon_payload_message->elements.at(1)).offset_v2);

  auto named_payload_name =
      fidl::flat::Name::Key(library.LookupLibrary("example"), "OnReceivePayload");
  auto type_named_payload = gen.CodedTypeFor(named_payload_name);
  ASSERT_NE(type_named_payload, nullptr);
  EXPECT_EQ("example_OnReceivePayload", type_named_payload->coded_name);
  EXPECT_TRUE(type_named_payload->is_coding_needed);
  EXPECT_EQ(1u, type_named_payload->size_v2);
  ASSERT_EQ(fidl::coded::Type::Kind::kStruct, type_named_payload->kind);
  auto type_named_payload_message = static_cast<const fidl::coded::StructType*>(type_named_payload);
  ASSERT_FALSE(type_named_payload_message->is_empty);
  EXPECT_EQ(type_named_payload_message->maybe_reference_type, nullptr);
  EXPECT_EQ("example/OnReceivePayload", type_named_payload_message->qname);
  EXPECT_EQ(1u, type_named_payload_message->elements.size());
  EXPECT_EQ(0u, field(type_named_payload_message->elements.at(0)).offset_v2);
}

TEST(CodedTypesGeneratorTests, GoodCodedTypesStructMessageErrorSyntax) {
  TestLibrary library(R"FIDL(library example;

protocol UseOfProtocol {
    strict Method() -> (struct {
        arg1 bool;
        arg2 bool;
    }) error uint32;
};
)FIDL");
  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();

  ASSERT_EQ(3u, gen.coded_types().size());

  auto type0 = gen.coded_types().at(0).get();
  EXPECT_EQ("example_UseOfProtocol_Method_ResultNullableRef", type0->coded_name);
  EXPECT_TRUE(type0->is_coding_needed);
  ASSERT_EQ(fidl::coded::Type::Kind::kUnion, type0->kind);
  auto type0_union = static_cast<const fidl::coded::UnionType*>(type0);
  ASSERT_EQ(fidl::types::Nullability::kNullable, type0_union->nullability);
  EXPECT_EQ(16u, type0->size_v2);
  ASSERT_EQ(2u, type0_union->fields.size());
  auto type0_field0 = type0_union->fields.at(0);
  EXPECT_EQ("example_UseOfProtocol_Method_Response", type0_field0.type->coded_name);
  auto type0_field1 = type0_union->fields.at(1);
  EXPECT_EQ("uint32", type0_field1.type->coded_name);

  auto type2 = gen.coded_types().at(1).get();
  EXPECT_EQ("bool", type2->coded_name);

  auto type3 = gen.coded_types().at(2).get();
  EXPECT_EQ("uint32", type3->coded_name);

  auto result_union_name =
      fidl::flat::Name::Key(library.LookupLibrary("example"), "UseOfProtocol_Method_Result");
  auto typed_result_union = gen.CodedTypeFor(result_union_name);
  EXPECT_EQ("example_UseOfProtocol_Method_Result", typed_result_union->coded_name);
  EXPECT_TRUE(typed_result_union->is_coding_needed);
  EXPECT_EQ(16u, typed_result_union->size_v2);
  ASSERT_EQ(fidl::coded::Type::Kind::kUnion, typed_result_union->kind);
  auto result_union_message = static_cast<const fidl::coded::UnionType*>(typed_result_union);
  EXPECT_EQ("example/UseOfProtocol_Method_Result", result_union_message->qname);
  ASSERT_EQ(2u, result_union_message->fields.size());

  auto anon_payload_name =
      fidl::flat::Name::Key(library.LookupLibrary("example"), "UseOfProtocol_Method_Response");
  auto type_anon_payload = gen.CodedTypeFor(anon_payload_name);
  ASSERT_NE(type_anon_payload, nullptr);
  EXPECT_EQ("example_UseOfProtocol_Method_Response", type_anon_payload->coded_name);
  EXPECT_TRUE(type_anon_payload->is_coding_needed);
  EXPECT_EQ(2u, type_anon_payload->size_v2);
  ASSERT_EQ(fidl::coded::Type::Kind::kStruct, type_anon_payload->kind);
  auto type_anon_payload_message = static_cast<const fidl::coded::StructType*>(type_anon_payload);
  ASSERT_FALSE(type_anon_payload_message->is_empty);
  EXPECT_EQ(type_anon_payload_message->maybe_reference_type, nullptr);
  EXPECT_EQ("example/UseOfProtocol_Method_Response", type_anon_payload_message->qname);
  ASSERT_EQ(2u, type_anon_payload_message->elements.size());
  EXPECT_EQ(0u, field(type_anon_payload_message->elements.at(0)).offset_v2);
  EXPECT_EQ(1u, field(type_anon_payload_message->elements.at(1)).offset_v2);
}

TEST(CodedTypesGeneratorTests, GoodCodedTypesTableMessage) {
  TestLibrary library(R"FIDL(library example;

type OnReceivePayload = table {
    1: arg bool;
};

protocol UseOfProtocol {
    Call(table {
        1: arg1 bool;
        2: arg2 bool;
    });
    -> OnReceive(OnReceivePayload);
};
)FIDL");
  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();

  ASSERT_EQ(1u, gen.coded_types().size());

  auto type0 = gen.coded_types().at(0).get();
  EXPECT_EQ("bool", type0->coded_name);

  auto anon_payload_name =
      fidl::flat::Name::Key(library.LookupLibrary("example"), "UseOfProtocolCallRequest");
  auto typed_anon_payload = gen.CodedTypeFor(anon_payload_name);
  ASSERT_NE(typed_anon_payload, nullptr);
  EXPECT_EQ("example_UseOfProtocolCallRequest", typed_anon_payload->coded_name);
  EXPECT_TRUE(typed_anon_payload->is_coding_needed);
  EXPECT_EQ(16u, typed_anon_payload->size_v2);
  ASSERT_EQ(fidl::coded::Type::Kind::kTable, typed_anon_payload->kind);
  auto anon_payload_message = static_cast<const fidl::coded::TableType*>(typed_anon_payload);
  EXPECT_EQ(fidl::types::Resourceness::kValue, anon_payload_message->resourceness);
  EXPECT_EQ("example/UseOfProtocolCallRequest", anon_payload_message->qname);
  ASSERT_EQ(2u, anon_payload_message->fields.size());
  EXPECT_EQ(1u, anon_payload_message->fields.at(0).type->size_v2);
  EXPECT_EQ(1u, anon_payload_message->fields.at(1).type->size_v2);

  auto named_payload_name =
      fidl::flat::Name::Key(library.LookupLibrary("example"), "OnReceivePayload");
  auto type_named_payload = gen.CodedTypeFor(named_payload_name);
  ASSERT_NE(type_named_payload, nullptr);
  EXPECT_EQ("example_OnReceivePayload", type_named_payload->coded_name);
  EXPECT_TRUE(type_named_payload->is_coding_needed);
  EXPECT_EQ(16u, type_named_payload->size_v2);
  ASSERT_EQ(fidl::coded::Type::Kind::kTable, type_named_payload->kind);
  auto type_named_payload_message = static_cast<const fidl::coded::TableType*>(type_named_payload);
  EXPECT_EQ(fidl::types::Resourceness::kValue, type_named_payload_message->resourceness);
  EXPECT_EQ("example/OnReceivePayload", type_named_payload_message->qname);
  ASSERT_EQ(1u, type_named_payload_message->fields.size());
  EXPECT_EQ(1u, type_named_payload_message->fields.at(0).type->size_v2);
}

TEST(CodedTypesGeneratorTests, GoodCodedTypesTableMessageErrorSyntax) {
  TestLibrary library(R"FIDL(library example;

protocol UseOfProtocol {
    strict Method() -> (table {
        1: arg1 bool;
        2: arg2 bool;
    }) error uint32;
};
)FIDL");
  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();

  ASSERT_EQ(3u, gen.coded_types().size());

  auto type0 = gen.coded_types().at(0).get();
  EXPECT_EQ("example_UseOfProtocol_Method_ResultNullableRef", type0->coded_name);
  EXPECT_TRUE(type0->is_coding_needed);
  ASSERT_EQ(fidl::coded::Type::Kind::kUnion, type0->kind);
  auto type0_union = static_cast<const fidl::coded::UnionType*>(type0);
  ASSERT_EQ(fidl::types::Nullability::kNullable, type0_union->nullability);
  EXPECT_EQ(16u, type0->size_v2);
  ASSERT_EQ(2u, type0_union->fields.size());
  auto type0_field0 = type0_union->fields.at(0);
  EXPECT_EQ("example_UseOfProtocol_Method_Response", type0_field0.type->coded_name);
  auto type0_field1 = type0_union->fields.at(1);
  EXPECT_EQ("uint32", type0_field1.type->coded_name);

  auto type2 = gen.coded_types().at(1).get();
  EXPECT_EQ("bool", type2->coded_name);

  auto type3 = gen.coded_types().at(2).get();
  EXPECT_EQ("uint32", type3->coded_name);

  auto anon_payload_name =
      fidl::flat::Name::Key(library.LookupLibrary("example"), "UseOfProtocol_Method_Response");
  auto type_anon_payload = gen.CodedTypeFor(anon_payload_name);
  ASSERT_NE(type_anon_payload, nullptr);
  EXPECT_EQ("example_UseOfProtocol_Method_Response", type_anon_payload->coded_name);
  EXPECT_TRUE(type_anon_payload->is_coding_needed);
  EXPECT_EQ(16u, type_anon_payload->size_v2);
  ASSERT_EQ(fidl::coded::Type::Kind::kTable, type_anon_payload->kind);
  auto type_anon_payload_message = static_cast<const fidl::coded::TableType*>(type_anon_payload);
  EXPECT_EQ(fidl::types::Resourceness::kValue, type_anon_payload_message->resourceness);
  EXPECT_EQ("example/UseOfProtocol_Method_Response", type_anon_payload_message->qname);
  ASSERT_EQ(2u, type_anon_payload_message->fields.size());
  EXPECT_EQ(1u, type_anon_payload_message->fields.at(0).type->size_v2);
  EXPECT_EQ(1u, type_anon_payload_message->fields.at(1).type->size_v2);
}

TEST(CodedTypesGeneratorTests, GoodCodedTypesUnionMessage) {
  TestLibrary library(R"FIDL(library example;

type OnReceivePayload = strict union {
    1: arg bool;
};

protocol UseOfProtocol {
    Call(flexible union {
        1: arg1 bool;
        2: arg2 bool;
    });
    -> OnReceive(OnReceivePayload);
};
)FIDL");
  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();

  ASSERT_EQ(3u, gen.coded_types().size());

  auto type2 = gen.coded_types().at(2).get();
  EXPECT_EQ("bool", type2->coded_name);

  auto anon_payload_name =
      fidl::flat::Name::Key(library.LookupLibrary("example"), "UseOfProtocolCallRequest");
  auto typed_anon_payload = gen.CodedTypeFor(anon_payload_name);
  EXPECT_EQ("example_UseOfProtocolCallRequest", typed_anon_payload->coded_name);
  EXPECT_TRUE(typed_anon_payload->is_coding_needed);
  EXPECT_EQ(16u, typed_anon_payload->size_v2);
  ASSERT_EQ(fidl::coded::Type::Kind::kUnion, typed_anon_payload->kind);
  auto anon_payload_message = static_cast<const fidl::coded::UnionType*>(typed_anon_payload);
  EXPECT_EQ(fidl::types::Nullability::kNonnullable, anon_payload_message->nullability);
  EXPECT_EQ(fidl::types::Resourceness::kValue, anon_payload_message->resourceness);
  EXPECT_EQ("example/UseOfProtocolCallRequest", anon_payload_message->qname);
  ASSERT_EQ(2u, anon_payload_message->fields.size());
  EXPECT_EQ(1u, anon_payload_message->fields.at(0).type->size_v2);
  EXPECT_EQ(1u, anon_payload_message->fields.at(1).type->size_v2);

  auto named_payload_name =
      fidl::flat::Name::Key(library.LookupLibrary("example"), "OnReceivePayload");
  auto type_named_payload = gen.CodedTypeFor(named_payload_name);
  ASSERT_NE(type_named_payload, nullptr);
  EXPECT_EQ("example_OnReceivePayload", type_named_payload->coded_name);
  EXPECT_TRUE(type_named_payload->is_coding_needed);
  EXPECT_EQ(16u, type_named_payload->size_v2);
  ASSERT_EQ(fidl::coded::Type::Kind::kUnion, type_named_payload->kind);
  auto type_named_payload_message = static_cast<const fidl::coded::UnionType*>(type_named_payload);
  EXPECT_EQ(fidl::types::Nullability::kNonnullable, type_named_payload_message->nullability);
  EXPECT_EQ(fidl::types::Resourceness::kValue, type_named_payload_message->resourceness);
  EXPECT_EQ("example/OnReceivePayload", type_named_payload_message->qname);
  ASSERT_EQ(1u, type_named_payload_message->fields.size());
  EXPECT_EQ(1u, type_named_payload_message->fields.at(0).type->size_v2);
}

TEST(CodedTypesGeneratorTests, GoodCodedTypesUnionMessageErrorSyntax) {
  TestLibrary library(R"FIDL(library example;

protocol UseOfProtocol {
    strict Method() -> (strict union {
        1: arg1 bool;
        2: arg2 bool;
    }) error uint32;
};
)FIDL");
  ASSERT_COMPILED(library);
  fidl::CodedTypesGenerator gen(library.compilation());
  gen.CompileCodedTypes();

  ASSERT_EQ(4u, gen.coded_types().size());

  auto type1 = gen.coded_types().at(1).get();
  EXPECT_EQ("example_UseOfProtocol_Method_ResultNullableRef", type1->coded_name);
  EXPECT_TRUE(type1->is_coding_needed);
  ASSERT_EQ(fidl::coded::Type::Kind::kUnion, type1->kind);
  auto type1_union = static_cast<const fidl::coded::UnionType*>(type1);
  ASSERT_EQ(fidl::types::Nullability::kNullable, type1_union->nullability);
  EXPECT_EQ(16u, type1->size_v2);
  ASSERT_EQ(2u, type1_union->fields.size());
  auto type1_field0 = type1_union->fields.at(0);
  EXPECT_EQ("example_UseOfProtocol_Method_Response", type1_field0.type->coded_name);
  auto type1_field1 = type1_union->fields.at(1);
  EXPECT_EQ("uint32", type1_field1.type->coded_name);

  auto type4 = gen.coded_types().at(2).get();
  EXPECT_EQ("bool", type4->coded_name);

  auto type5 = gen.coded_types().at(3).get();
  EXPECT_EQ("uint32", type5->coded_name);

  auto result_union_name =
      fidl::flat::Name::Key(library.LookupLibrary("example"), "UseOfProtocol_Method_Result");
  auto typed_result_union = gen.CodedTypeFor(result_union_name);
  EXPECT_EQ("example_UseOfProtocol_Method_Result", typed_result_union->coded_name);
  EXPECT_TRUE(typed_result_union->is_coding_needed);
  EXPECT_EQ(16u, typed_result_union->size_v2);
  ASSERT_EQ(fidl::coded::Type::Kind::kUnion, typed_result_union->kind);
  auto result_union_message = static_cast<const fidl::coded::UnionType*>(typed_result_union);
  EXPECT_EQ("example/UseOfProtocol_Method_Result", result_union_message->qname);
  ASSERT_EQ(2u, result_union_message->fields.size());

  auto anon_payload_name =
      fidl::flat::Name::Key(library.LookupLibrary("example"), "UseOfProtocol_Method_Response");
  auto type_anon_payload = gen.CodedTypeFor(anon_payload_name);
  ASSERT_NE(type_anon_payload, nullptr);
  EXPECT_EQ("example_UseOfProtocol_Method_Response", type_anon_payload->coded_name);
  EXPECT_TRUE(type_anon_payload->is_coding_needed);
  EXPECT_EQ(16u, type_anon_payload->size_v2);
  ASSERT_EQ(fidl::coded::Type::Kind::kUnion, type_anon_payload->kind);
  auto type_anon_payload_message = static_cast<const fidl::coded::UnionType*>(type_anon_payload);
  EXPECT_EQ(fidl::types::Nullability::kNonnullable, type_anon_payload_message->nullability);
  EXPECT_EQ(fidl::types::Resourceness::kValue, type_anon_payload_message->resourceness);
  EXPECT_EQ("example/UseOfProtocol_Method_Response", type_anon_payload_message->qname);
  ASSERT_EQ(2u, type_anon_payload_message->fields.size());
  EXPECT_EQ(1u, type_anon_payload_message->fields.at(0).type->size_v2);
  EXPECT_EQ(1u, type_anon_payload_message->fields.at(1).type->size_v2);
}

}  // namespace
