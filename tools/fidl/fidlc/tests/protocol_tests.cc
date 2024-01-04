// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "tools/fidl/fidlc/include/fidl/diagnostics.h"
#include "tools/fidl/fidlc/include/fidl/flat/types.h"
#include "tools/fidl/fidlc/include/fidl/flat_ast.h"
#include "tools/fidl/fidlc/tests/test_library.h"

namespace {

TEST(ProtocolTests, GoodValidEmptyProtocol) {
  TestLibrary library(R"FIDL(library example;

protocol Empty {};
)FIDL");
  ASSERT_COMPILED(library);

  auto protocol = library.LookupProtocol("Empty");
  ASSERT_NE(protocol, nullptr);

  EXPECT_EQ(protocol->methods.size(), 0u);
  EXPECT_EQ(protocol->openness, fidl::types::Openness::kOpen);
  EXPECT_EQ(protocol->all_methods.size(), 0u);
}

TEST(ProtocolTests, GoodValidEmptyOpenProtocol) {
  TestLibrary library(R"FIDL(library example;

open protocol Empty {};
)FIDL");
  ASSERT_COMPILED(library);

  auto protocol = library.LookupProtocol("Empty");
  ASSERT_NE(protocol, nullptr);

  EXPECT_EQ(protocol->methods.size(), 0u);
  EXPECT_EQ(protocol->openness, fidl::types::Openness::kOpen);
  EXPECT_EQ(protocol->all_methods.size(), 0u);
}

TEST(ProtocolTests, GoodValidEmptyAjarProtocol) {
  TestLibrary library(R"FIDL(library example;

ajar protocol Empty {};
)FIDL");
  ASSERT_COMPILED(library);

  auto protocol = library.LookupProtocol("Empty");
  ASSERT_NE(protocol, nullptr);

  EXPECT_EQ(protocol->methods.size(), 0u);
  EXPECT_EQ(protocol->openness, fidl::types::Openness::kAjar);
  EXPECT_EQ(protocol->all_methods.size(), 0u);
}

TEST(ProtocolTests, GoodValidEmptyClosedProtocol) {
  TestLibrary library(R"FIDL(library example;

closed protocol Empty {};
)FIDL");
  ASSERT_COMPILED(library);

  auto protocol = library.LookupProtocol("Empty");
  ASSERT_NE(protocol, nullptr);

  EXPECT_EQ(protocol->methods.size(), 0u);
  EXPECT_EQ(protocol->openness, fidl::types::Openness::kClosed);
  EXPECT_EQ(protocol->all_methods.size(), 0u);
}

TEST(ProtocolTests, BadEmptyStrictProtocol) {
  TestLibrary library(R"FIDL(
library example;

strict protocol Empty {};

)FIDL");
  library.ExpectFail(fidl::ErrExpectedDeclaration, "strict");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadEmptyFlexibleProtocol) {
  TestLibrary library(R"FIDL(
library example;

flexible protocol Empty {};

)FIDL");
  library.ExpectFail(fidl::ErrExpectedDeclaration, "flexible");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadOpenMissingProtocolToken) {
  TestLibrary library(R"FIDL(
library example;

open Empty {};

)FIDL");
  library.ExpectFail(fidl::ErrUnexpectedIdentifier,
                     fidl::Token::KindAndSubkind(fidl::Token::Kind::kIdentifier),
                     fidl::Token::KindAndSubkind(fidl::Token::Subkind::kProtocol));
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadAjarMissingProtocolToken) {
  TestLibrary library(R"FIDL(
library example;

ajar Empty {};

)FIDL");
  library.ExpectFail(fidl::ErrUnexpectedIdentifier,
                     fidl::Token::KindAndSubkind(fidl::Token::Kind::kIdentifier),
                     fidl::Token::KindAndSubkind(fidl::Token::Subkind::kProtocol));
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadClosedMissingProtocolToken) {
  TestLibrary library(R"FIDL(
library example;

closed Empty {};

)FIDL");
  library.ExpectFail(fidl::ErrUnexpectedIdentifier,
                     fidl::Token::KindAndSubkind(fidl::Token::Kind::kIdentifier),
                     fidl::Token::KindAndSubkind(fidl::Token::Subkind::kProtocol));
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadEmptyProtocolMember) {
  TestLibrary library(R"FIDL(
library example;

protocol Example {
  ;
};

)FIDL");
  library.ExpectFail(fidl::ErrInvalidProtocolMember);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, GoodValidProtocolComposition) {
  TestLibrary library(R"FIDL(library example;

protocol A {
    MethodA();
};

protocol B {
    compose A;
    MethodB();
};

protocol C {
    compose A;
    MethodC();
};

protocol D {
    compose B;
    compose C;
    MethodD();
};
)FIDL");
  ASSERT_COMPILED(library);

  auto protocol_a = library.LookupProtocol("A");
  ASSERT_NE(protocol_a, nullptr);
  EXPECT_EQ(protocol_a->methods.size(), 1u);
  EXPECT_EQ(protocol_a->all_methods.size(), 1u);

  auto protocol_b = library.LookupProtocol("B");
  ASSERT_NE(protocol_b, nullptr);
  EXPECT_EQ(protocol_b->methods.size(), 1u);
  EXPECT_EQ(protocol_b->all_methods.size(), 2u);

  auto protocol_c = library.LookupProtocol("C");
  ASSERT_NE(protocol_c, nullptr);
  EXPECT_EQ(protocol_c->methods.size(), 1u);
  EXPECT_EQ(protocol_c->all_methods.size(), 2u);

  auto protocol_d = library.LookupProtocol("D");
  ASSERT_NE(protocol_d, nullptr);
  EXPECT_EQ(protocol_d->methods.size(), 1u);
  EXPECT_EQ(protocol_d->all_methods.size(), 4u);
}

TEST(ProtocolTests, GoodValidOpenClosedProtocolComposition) {
  TestLibrary library(R"FIDL(
library example;

closed protocol Closed {};
ajar protocol Ajar {};
open protocol Open {};

closed protocol ComposeInClosed {
  compose Closed;
};

ajar protocol ComposeInAjar {
  compose Closed;
  compose Ajar;
};

open protocol ComposeInOpen {
  compose Closed;
  compose Ajar;
  compose Open;
};

)FIDL");
  ASSERT_COMPILED(library);

  auto compose_in_closed = library.LookupProtocol("ComposeInClosed");
  ASSERT_NE(compose_in_closed, nullptr);
  EXPECT_EQ(compose_in_closed->composed_protocols.size(), 1u);

  auto compose_in_ajar = library.LookupProtocol("ComposeInAjar");
  ASSERT_NE(compose_in_ajar, nullptr);
  EXPECT_EQ(compose_in_ajar->composed_protocols.size(), 2u);

  auto compose_in_open = library.LookupProtocol("ComposeInOpen");
  ASSERT_NE(compose_in_open, nullptr);
  EXPECT_EQ(compose_in_open->composed_protocols.size(), 3u);
}

TEST(ProtocolTests, BadInvalidComposeOpenInClosed) {
  TestLibrary library(R"FIDL(
library example;

open protocol Composed {};

closed protocol Composing {
  compose Composed;
};

)FIDL");
  library.ExpectFail(fidl::ErrComposedProtocolTooOpen, fidl::types::Openness::kClosed, "Composing",
                     fidl::types::Openness::kOpen, "Composed");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadInvalidComposeAjarInClosed) {
  TestLibrary library(R"FIDL(
library example;

ajar protocol Composed {};

closed protocol Composing {
  compose Composed;
};

)FIDL");
  library.ExpectFail(fidl::ErrComposedProtocolTooOpen, fidl::types::Openness::kClosed, "Composing",
                     fidl::types::Openness::kAjar, "Composed");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadInvalidComposeOpenInAjar) {
  TestLibrary library;
  library.AddFile("bad/fi-0114.test.fidl");
  library.ExpectFail(fidl::ErrComposedProtocolTooOpen, fidl::types::Openness::kAjar, "Composing",
                     fidl::types::Openness::kOpen, "Composed");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadModifierStrictOnCompose) {
  TestLibrary library(R"FIDL(
library example;

protocol A {};

protocol B {
  strict compose A;
};

)FIDL");
  library.ExpectFail(fidl::ErrInvalidProtocolMember);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadModifierFlexibleOnCompose) {
  TestLibrary library(R"FIDL(
library example;

protocol A {};

protocol B {
  flexible compose A;
};

)FIDL");
  library.ExpectFail(fidl::ErrInvalidProtocolMember);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadModifierStrictOnInvalidMember) {
  TestLibrary library(R"FIDL(
library example;

protocol Example {
  strict;
};

)FIDL");
  library.ExpectFail(fidl::ErrInvalidProtocolMember);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadModifierFlexibleOnInvalidMember) {
  TestLibrary library(R"FIDL(
library example;

protocol Example {
  flexible;
};

)FIDL");
  library.ExpectFail(fidl::ErrInvalidProtocolMember);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadColonNotSupported) {
  TestLibrary library(R"FIDL(
library example;

protocol Parent {};
protocol Child : Parent {};

)FIDL");
  library.ExpectFail(fidl::ErrUnexpectedTokenOfKind,
                     fidl::Token::KindAndSubkind(fidl::Token::Kind::kColon),
                     fidl::Token::KindAndSubkind(fidl::Token::Kind::kLeftCurly));
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadDocCommentOutsideAttributelist) {
  TestLibrary library(R"FIDL(
library example;

protocol WellDocumented {
    Method();
    /// Misplaced doc comment
};

)FIDL");
  library.ExpectFail(fidl::ErrInvalidProtocolMember);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, GoodAttachAttributesToCompose) {
  TestLibrary library(R"FIDL(library example;

protocol ParentA {
    ParentMethodA();
};

protocol ParentB {
    ParentMethodB();
};

protocol Child {
    @this_is_allowed
    compose ParentA;
    /// This is also allowed.
    compose ParentB;
    ChildMethod();
};
)FIDL");
  ASSERT_COMPILED(library);

  auto child_protocol = library.LookupProtocol("Child");
  ASSERT_NE(child_protocol, nullptr);
  EXPECT_EQ(child_protocol->methods.size(), 1u);
  EXPECT_EQ(child_protocol->all_methods.size(), 3u);
  ASSERT_EQ(child_protocol->composed_protocols.size(), 2u);
  EXPECT_EQ(child_protocol->composed_protocols.front().attributes->attributes.size(), 1u);
  EXPECT_EQ(child_protocol->composed_protocols.front().attributes->attributes.front()->name.data(),
            "this_is_allowed");
  EXPECT_EQ(child_protocol->composed_protocols.back().attributes->attributes.size(), 1u);
  EXPECT_EQ(child_protocol->composed_protocols.back().attributes->attributes.front()->name.data(),
            "doc");
  EXPECT_EQ(child_protocol->composed_protocols.back().attributes->attributes.front()->span.data(),
            "/// This is also allowed.");
  ASSERT_EQ(child_protocol->composed_protocols.back().attributes->attributes.front()->args.size(),
            1u);
  EXPECT_TRUE(child_protocol->composed_protocols.back()
                  .attributes->attributes.front()
                  ->args.front()
                  ->value->IsResolved());
}

TEST(ProtocolTests, BadCannotComposeYourself) {
  TestLibrary library(R"FIDL(
library example;

protocol Narcisse {
    compose Narcisse;
};

)FIDL");
  library.ExpectFail(fidl::ErrIncludeCycle, "protocol 'Narcisse' -> protocol 'Narcisse'");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadCannotMutuallyCompose) {
  TestLibrary library;
  library.AddFile("bad/fi-0057-b.test.fidl");

  library.ExpectFail(fidl::ErrIncludeCycle, "protocol 'Yang' -> protocol 'Yin' -> protocol 'Yang'");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadCannotComposeSameProtocolTwice) {
  TestLibrary library;
  library.AddFile("bad/fi-0047.test.fidl");
  library.ExpectFail(fidl::ErrProtocolComposedMultipleTimes, "bad/fi-0047.test.fidl:11:13");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadCannotComposeMissingProtocol) {
  TestLibrary library(R"FIDL(
library example;

protocol Child {
    compose MissingParent;
};

)FIDL");
  library.ExpectFail(fidl::ErrNameNotFound, "MissingParent", "example");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadCannotComposeNonProtocol) {
  TestLibrary library;
  library.AddFile("bad/fi-0073.test.fidl");
  library.ExpectFail(fidl::ErrComposingNonProtocol);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadCannotUseOrdinalsInProtocolDeclaration) {
  TestLibrary library(R"FIDL(
library example;

protocol NoMoreOrdinals {
    42: NiceTry();
};

)FIDL");
  library.ExpectFail(fidl::ErrInvalidProtocolMember);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadEmptyNamedItem) {
  TestLibrary library;
  library.AddFile("bad/fi-0020.test.fidl");
  library.ExpectFail(fidl::ErrInvalidProtocolMember);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadNoOtherPragmaThanCompose) {
  TestLibrary library(R"FIDL(
library example;

protocol Wrong {
    not_compose Something;
};

)FIDL");
  library.ExpectFail(fidl::ErrInvalidProtocolMember);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadDuplicateMethodNames) {
  TestLibrary library(R"FIDL(
library example;

protocol MyProtocol {
    MyMethod();
    MyMethod();
};
)FIDL");
  library.ExpectFail(fidl::ErrNameCollision, fidl::flat::Element::Kind::kProtocolMethod, "MyMethod",
                     fidl::flat::Element::Kind::kProtocolMethod, "example.fidl:5:5");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadDuplicateMethodNamesFromImmediateComposition) {
  TestLibrary library(R"FIDL(
library example;

protocol MyChildProtocol {
    MyMethod();
};

protocol MyProtocol {
    compose MyChildProtocol;
    MyMethod();
};
)FIDL");
  library.ExpectFail(fidl::ErrNameCollision, fidl::flat::Element::Kind::kProtocolMethod, "MyMethod",
                     fidl::flat::Element::Kind::kProtocolMethod, "example.fidl:5:5");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadDuplicateMethodNamesFromMultipleComposition) {
  TestLibrary library(R"FIDL(
library example;

protocol A {
    Method();
};

protocol B {
    Method();
};

protocol C {
    compose A;
    compose B;
};
)FIDL");
  library.ExpectFail(fidl::ErrNameCollision, fidl::flat::Element::Kind::kProtocolMethod, "Method",
                     fidl::flat::Element::Kind::kProtocolMethod, "example.fidl:5:5");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadDuplicateMethodNamesFromNestedComposition) {
  TestLibrary library(R"FIDL(
library example;

protocol A {
    MethodA();
};

protocol B {
    compose A;
    MethodB();
};

protocol C {
    compose A;
    MethodC();
};

protocol D {
    compose B;
    compose C;
    MethodD();
    MethodA();
};
)FIDL");
  library.ExpectFail(fidl::ErrNameCollision, fidl::flat::Element::Kind::kProtocolMethod, "MethodA",
                     fidl::flat::Element::Kind::kProtocolMethod, "example.fidl:5:5");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

// See GetGeneratedOrdinal64ForTesting in test_library.h
// See GetGeneratedOrdinal64ForTesting in test_library.h
TEST(ProtocolTests, BadComposedProtocolsHaveClashingOrdinals) {
  TestLibrary library(R"FIDL(
library methodhasher;

protocol SpecialComposed {
   ClashOne();
};

protocol Special {
    compose SpecialComposed;
    ClashTwo();
};
)FIDL");
  library.ExpectFail(fidl::ErrDuplicateMethodOrdinal, "example.fidl:5:4");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadRequestMustBeProtocol) {
  TestLibrary library;
  library.AddFile("bad/fi-0157.test.fidl");
  library.ExpectFail(fidl::ErrMustBeAProtocol, "server_end");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadRequestMustBeProtocolWithOptional) {
  TestLibrary library(R"FIDL(
library example;

type MyStruct = struct {};
alias ServerEnd = server_end:<MyStruct, optional>;
)FIDL");
  library.ExpectFail(fidl::ErrMustBeAProtocol, "server_end");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadRequestMustBeParameterized) {
  TestLibrary library;
  library.AddFile("bad/fi-0168.test.fidl");
  library.ExpectFail(fidl::ErrProtocolConstraintRequired, "server_end");
  ASSERT_COMPILER_DIAGNOSTICS(library);
  EXPECT_EQ(library.errors()[0]->span.data(), "server_end");
}

TEST(ProtocolTests, BadRequestMustContainProtocol) {
  TestLibrary library(R"FIDL(
library example;

protocol MyProtocol {
    MyMethod(resource struct { server server_end; });
};
)FIDL");
  library.ExpectFail(fidl::ErrProtocolConstraintRequired, "server_end");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadRequestCannotHaveSize) {
  TestLibrary library(R"FIDL(
library example;

protocol P {};
type S = struct {
    p server_end:<P,0>;
};
)FIDL");
  library.ExpectFail(fidl::ErrUnexpectedConstraint, "server_end");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadDuplicateParameterName) {
  TestLibrary library(R"FIDL(
library example;

protocol P {
  MethodWithDuplicateParams(struct {foo uint8; foo uint8; });
};
)FIDL");
  library.ExpectFail(fidl::ErrNameCollision, fidl::flat::Element::Kind::kStructMember, "foo",
                     fidl::flat::Element::Kind::kStructMember, "example.fidl:5:37");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadParameterizedTypedChannel) {
  TestLibrary library(R"FIDL(
library example;

protocol MyProtocol {};

type Foo = resource struct {
  foo client_end<MyProtocol>;
};
)FIDL");
  library.ExpectFail(fidl::ErrWrongNumberOfLayoutParameters, "client_end", 0, 1);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadTooManyConstraintsTypedChannel) {
  TestLibrary library(R"FIDL(
library example;

protocol MyProtocol {};

type Foo = resource struct {
  foo client_end:<MyProtocol, optional, 1, 2>;
};
)FIDL");
  library.ExpectFail(fidl::ErrTooManyConstraints, "client_end", 2, 4);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, GoodTypedChannels) {
  TestLibrary library(R"FIDL(library example;

protocol MyProtocol {};

type Foo = resource struct {
    a client_end:MyProtocol;
    b client_end:<MyProtocol, optional>;
    c server_end:MyProtocol;
    d server_end:<MyProtocol, optional>;
};
)FIDL");
  ASSERT_COMPILED(library);

  auto container = library.LookupStruct("Foo");
  ASSERT_NE(container, nullptr);
  ASSERT_EQ(container->members.size(), 4u);

  size_t i = 0;

  auto a_type_base = container->members[i++].type_ctor->type;
  ASSERT_EQ(a_type_base->kind, fidl::flat::Type::Kind::kTransportSide);
  const auto* a_type = static_cast<const fidl::flat::TransportSideType*>(a_type_base);
  EXPECT_EQ(a_type->end, fidl::flat::TransportSide::kClient);
  EXPECT_EQ(a_type->nullability, fidl::types::Nullability::kNonnullable);

  auto b_type_base = container->members[i++].type_ctor->type;
  ASSERT_EQ(b_type_base->kind, fidl::flat::Type::Kind::kTransportSide);
  const auto* b_type = static_cast<const fidl::flat::TransportSideType*>(b_type_base);
  EXPECT_EQ(b_type->end, fidl::flat::TransportSide::kClient);
  EXPECT_EQ(b_type->nullability, fidl::types::Nullability::kNullable);

  auto c_type_base = container->members[i++].type_ctor->type;
  ASSERT_EQ(c_type_base->kind, fidl::flat::Type::Kind::kTransportSide);
  const auto* c_type = static_cast<const fidl::flat::TransportSideType*>(c_type_base);
  EXPECT_EQ(c_type->end, fidl::flat::TransportSide::kServer);
  EXPECT_EQ(c_type->nullability, fidl::types::Nullability::kNonnullable);

  auto d_type_base = container->members[i++].type_ctor->type;
  ASSERT_EQ(d_type_base->kind, fidl::flat::Type::Kind::kTransportSide);
  const auto* d_type = static_cast<const fidl::flat::TransportSideType*>(d_type_base);
  EXPECT_EQ(d_type->end, fidl::flat::TransportSide::kServer);
  EXPECT_EQ(d_type->nullability, fidl::types::Nullability::kNullable);
}

TEST(ProtocolTests, GoodPartialTypedChannelConstraints) {
  TestLibrary library(R"FIDL(library example;

protocol MyProtocol {};

alias ClientEnd = client_end:MyProtocol;
alias ServerEnd = server_end:MyProtocol;

type Foo = resource struct {
    a ClientEnd;
    b ClientEnd:optional;
    c ServerEnd;
    d ServerEnd:optional;
};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(ProtocolTests, BadMethodStructLayoutDefaultMember) {
  TestLibrary library(R"FIDL(
library example;

protocol MyProtocol {
  MyMethod(struct {
    @allow_deprecated_struct_defaults
    foo uint8 = 1;
  });
};
)FIDL");
  library.ExpectFail(fidl::ErrPayloadStructHasDefaultMembers);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadMethodEmptyPayloadStruct) {
  TestLibrary library;
  library.AddFile("bad/fi-0077-a.test.fidl");
  library.ExpectFail(fidl::ErrEmptyPayloadStructs, "MyMethod");
  library.ExpectFail(fidl::ErrEmptyPayloadStructs, "MyMethod");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, GoodMethodAbsentPayloadStruct) {
  TestLibrary library;
  library.AddFile("good/fi-0077-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ProtocolTests, BadEventEmptyPayloadStruct) {
  TestLibrary library;
  library.AddFile("bad/fi-0077-b.test.fidl");
  library.ExpectFail(fidl::ErrEmptyPayloadStructs, "MyEvent");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, GoodEventAbsentPayloadStruct) {
  TestLibrary library;
  library.AddFile("good/fi-0077-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ProtocolTests, BadMethodEnumLayout) {
  TestLibrary library;
  library.AddFile("bad/fi-0074.test.fidl");
  library.ExpectFail(fidl::ErrInvalidMethodPayloadLayoutClass, fidl::flat::Decl::Kind::kEnum);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, GoodMethodAbsentResponseWithError) {
  TestLibrary library(R"FIDL(
library example;

protocol MyProtocol {
    MyMethod() -> () error uint32;
};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(ProtocolTests, BadMethodEmptyStructResponseWithError) {
  TestLibrary library(R"FIDL(
library example;

protocol MyProtocol {
    MyMethod() -> (struct {}) error uint32;
};
)FIDL");
  library.ExpectFail(fidl::ErrEmptyPayloadStructs, "MyMethod");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, GoodMethodNamedTypeRequest) {
  TestLibrary library(R"FIDL(
library example;

type MyStruct = struct{
  a bool;
};

protocol MyProtocol {
    MyMethodOneWay(MyStruct);
    MyMethodTwoWay(MyStruct) -> ();
};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(ProtocolTests, GoodMethodNamedTypeResponse) {
  TestLibrary library(R"FIDL(
library example;

type MyStruct = struct{
  a bool;
};

protocol MyProtocol {
  MyMethod() -> (MyStruct);
    -> OnMyEvent(MyStruct);
};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(ProtocolTests, GoodMethodNamedTypeResultPayload) {
  TestLibrary library(R"FIDL(
library example;

type MyStruct = struct{
  a bool;
};

protocol MyProtocol {
  MyMethod() -> (MyStruct) error uint32;
};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(ProtocolTests, GoodMethodNamedAlias) {
  TestLibrary library(R"FIDL(
library example;

type MyStruct = struct {
  a bool;
};

alias MyStructAlias = MyStruct;
alias MyAliasAlias = MyStructAlias;

protocol MyProtocol {
    MyMethod(MyStructAlias) -> (MyAliasAlias);
};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(ProtocolTests, BadMethodNamedEmptyPayloadStruct) {
  TestLibrary library(R"FIDL(
library example;

type MyStruct = struct{};

protocol MyProtocol {
    MyMethod(MyStruct) -> (MyStruct);
};
)FIDL");
  library.ExpectFail(fidl::ErrEmptyPayloadStructs, "MyMethod");
  library.ExpectFail(fidl::ErrEmptyPayloadStructs, "MyMethod");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadMethodNamedDefaultValueStruct) {
  TestLibrary library;
  library.AddFile("bad/fi-0084.test.fidl");
  library.ExpectFail(fidl::ErrPayloadStructHasDefaultMembers);
  library.ExpectFail(fidl::ErrPayloadStructHasDefaultMembers);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadMethodNamedInvalidHandle) {
  TestLibrary library(R"FIDL(
library example;

type ObjType = strict enum : uint32 {
    NONE = 0;
    VMO = 3;
};

type Rights = strict bits : uint32 {
    TRANSFER = 1;
};

resource_definition Handle : uint32 {
    properties {
        subtype ObjType;
        rights Rights;
    };
};

protocol MyProtocol {
    MyMethod(Handle);
};
)FIDL");
  library.ExpectFail(fidl::ErrInvalidMethodPayloadType, "example/Handle");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadMethodNamedInvalidAlias) {
  TestLibrary library(R"FIDL(
library example;

type ObjType = strict enum : uint32 {
    NONE = 0;
    VMO = 3;
};

type Rights = strict bits : uint32 {
    TRANSFER = 1;
};

resource_definition Handle : uint32 {
    properties {
        subtype ObjType;
        rights Rights;
    };
};

alias MyPrimAlias = bool;
alias MyHandleAlias = Handle;
alias MyVectorAlias = vector<MyPrimAlias>;
alias MyAliasAlias = MyVectorAlias:optional;

protocol MyProtocol {
    strict MyMethod(MyPrimAlias) -> (MyHandleAlias);
    strict MyOtherMethod(MyVectorAlias) -> (MyAliasAlias);
};
)FIDL");

  library.ExpectFail(fidl::ErrInvalidMethodPayloadType, "bool");
  library.ExpectFail(fidl::ErrInvalidMethodPayloadType, "example/Handle");
  library.ExpectFail(fidl::ErrInvalidMethodPayloadType, "vector<bool>");
  // TODO(https://fxbug.dev/93999): Should be "vector<bool>:optional".
  library.ExpectFail(fidl::ErrInvalidMethodPayloadType, "vector<bool>?");

  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadMethodNamedInvalidKind) {
  TestLibrary library(R"FIDL(
library example;

protocol MyOtherProtocol {
  MyOtherMethod();
};

service MyService {
  my_other_protocol client_end:MyOtherProtocol;
};

protocol MyProtocol {
    MyMethod(MyOtherProtocol) -> (MyService);
};
)FIDL");
  library.ExpectFail(fidl::ErrExpectedType);
  library.ExpectFail(fidl::ErrExpectedType);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, GoodMethodTableRequest) {
  TestLibrary library(R"FIDL(
library example;

protocol MyOtherProtocol {};

type MyTable = resource table {
  1: a client_end:<MyProtocol>;
};

protocol MyProtocol {
  MyMethodOneWay(table {
    1: b bool;
  });
  MyMethodTwoWay(MyTable) -> ();
};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(ProtocolTests, GoodMethodTableResponse) {
  TestLibrary library(R"FIDL(
library example;

protocol MyOtherProtocol {};

type MyTable = resource table {
  1: a client_end:<MyProtocol>;
};

protocol MyProtocol {
  MyMethod() -> (table {
    1: b bool;
  });
  -> OnMyEvent(MyTable);
};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(ProtocolTests, GoodMethodTableResultPayload) {
  TestLibrary library(R"FIDL(
library example;

protocol MyOtherProtocol {};

type MyTable = resource table {
  1: a client_end:<MyProtocol>;
};

protocol MyProtocol {
  MyMethod() -> (MyTable) error uint32;
  MyAnonResponseMethod() -> (table {
    1: b bool;
  }) error uint32;
};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(ProtocolTests, GoodMethodUnionRequest) {
  TestLibrary library(R"FIDL(
library example;

protocol MyOtherProtocol {};

type MyUnion = strict resource union {
  1: a client_end:<MyProtocol>;
};

protocol MyProtocol {
  MyMethodOneWay(flexible union {
    1: b bool;
  });
  MyMethodTwoWay(MyUnion) -> ();
};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(ProtocolTests, GoodMethodUnionResponse) {
  TestLibrary library(R"FIDL(
library example;

protocol MyOtherProtocol {};

type MyUnion = strict resource union {
  1: a client_end:<MyProtocol>;
};

protocol MyProtocol {
  MyMethod() -> (flexible union {
    1: b bool;
  });
  -> OnMyEvent(MyUnion);
};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(ProtocolTests, GoodMethodUnionResultPayload) {
  TestLibrary library(R"FIDL(
library example;

protocol MyOtherProtocol {};

type MyUnion = strict resource union {
  1: a client_end:<MyProtocol>;
};

protocol MyProtocol {
  MyMethod() -> (MyUnion) error uint32;
  MyAnonResponseMethod() -> (flexible union {
    1: b bool;
  }) error uint32;
};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(ProtocolTests, BadEventErrorSyntax) {
  TestLibrary library;
  library.AddFile("bad/fi-0119.test.fidl");
  library.ExpectFail(fidl::ErrEventErrorSyntaxDeprecated, "OnMyEvent");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadDisallowedRequestType) {
  TestLibrary library;
  library.AddFile("bad/fi-0075.test.fidl");
  library.ExpectFail(fidl::ErrInvalidMethodPayloadType, "uint32");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadInvalidRequestType) {
  TestLibrary library(R"FIDL(
library example;

protocol MyProtocol {
    MyMethod(box);
};
)FIDL");
  library.ExpectFail(fidl::ErrWrongNumberOfLayoutParameters, "box", 1, 0);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadDisallowedResponseType) {
  TestLibrary library(R"FIDL(
library example;

protocol MyProtocol {
    MyMethod() -> (uint32);
};
)FIDL");
  library.ExpectFail(fidl::ErrInvalidMethodPayloadType, "uint32");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadInvalidResponseType) {
  TestLibrary library(R"FIDL(
library example;

protocol MyProtocol {
    MyMethod() -> (box);
};
)FIDL");
  library.ExpectFail(fidl::ErrWrongNumberOfLayoutParameters, "box", 1, 0);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadDisallowedSuccessType) {
  TestLibrary library(R"FIDL(
library example;

protocol MyProtocol {
    MyMethod() -> (uint32) error uint32;
};
)FIDL");
  library.ExpectFail(fidl::ErrInvalidMethodPayloadType, "uint32");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ProtocolTests, BadInvalidSuccessType) {
  TestLibrary library(R"FIDL(
library example;

protocol MyProtocol {
    MyMethod() -> (box) error uint32;
};
)FIDL");
  library.ExpectFail(fidl::ErrWrongNumberOfLayoutParameters, "box", 1, 0);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

// TODO(https://fxbug.dev/93542): add bad `:optional` message body tests here.

}  // namespace
