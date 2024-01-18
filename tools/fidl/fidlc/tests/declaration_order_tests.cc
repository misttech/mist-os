// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zircon/assert.h>

#include <algorithm>
#include <map>
#include <random>
#include <string>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "tools/fidl/fidlc/src/flat_ast.h"
#include "tools/fidl/fidlc/src/names.h"
#include "tools/fidl/fidlc/tests/test_library.h"

namespace fidlc {
namespace {

using ::testing::HasSubstr;

// The calculated declaration order is a product of both the inter-type dependency relationships,
// and an ordering among the type names. To eliminate the effect of name ordering and exclusively
// test dependency ordering, this utility manufactures random names for the types tested.
class Namer {
 public:
  Namer() = default;

  std::string mangle(std::string input) {
    std::size_t start_pos = 0;
    std::size_t max_length = 0;
    while ((start_pos = input.find_first_of('#', start_pos)) != std::string::npos) {
      std::size_t end_pos = input.find_first_of('#', start_pos + 1);
      ZX_ASSERT(end_pos != std::string::npos);
      std::size_t key_len = end_pos - start_pos;
      max_length = std::max(max_length, key_len);
      start_pos = end_pos + 1;
    }
    std::size_t normalize_length = max_length + 5;
    while ((start_pos = input.find_first_of('#')) != std::string::npos) {
      std::size_t end_pos = input.find_first_of('#', start_pos + 1);
      auto key = input.substr(start_pos + 1, end_pos - start_pos - 1);
      if (vars_.find(key) == vars_.end()) {
        vars_[key] = random_prefix(key, normalize_length);
      }
      auto replacement = vars_.at(key);
      input.replace(start_pos, end_pos - start_pos + 1, replacement);
    }
    return input;
  }

  const char* of(std::string_view key) const { return vars_.find(key)->second.c_str(); }

 private:
  std::string random_prefix(std::string label, std::size_t up_to) {
    // normalize any name to at least |up_to| characters, by adding random prefix
    static std::string characters = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    constexpr size_t kSeed = 1337;
    static std::default_random_engine gen(kSeed);
    static std::uniform_int_distribution<size_t> distribution(0, characters.size() - 1);
    if (label.size() < up_to - 1) {
      label = "_" + label;
    }
    while (label.size() < up_to) {
      label.insert(0, 1, characters[distribution(gen)]);
    }
    return label;
  }

  // Use transparent comparator std::less<> to allow std::string_view lookups.
  std::map<std::string, std::string, std::less<>> vars_;
};

constexpr int kRepeatTestCount = 100;

// This test ensures that there are no unused anonymous structs in the
// declaration order output.
TEST(DeclarationOrderTests, GoodNoUnusedAnonymousNames) {
  for (int i = 0; i < kRepeatTestCount; i++) {
    Namer namer;
    auto source = namer.mangle(R"FIDL(
library example;

protocol #Protocol# {
    strict Method() -> ();
};
)FIDL");
    TestLibrary library(source);
    ASSERT_COMPILED(library);
    auto decl_order = library.declaration_order();
    ASSERT_EQ(decl_order.size(), 1u);
    ASSERT_EQ(decl_order[0]->name.decl_name(), namer.of("Protocol"));
  }
}

TEST(DeclarationOrderTests, GoodNonnullableRef) {
  for (int i = 0; i < kRepeatTestCount; i++) {
    Namer namer;
    auto source = namer.mangle(R"FIDL(
library example;

type #Request# = struct {
  req array<#Element#, 4>;
};

type #Element# = struct {};

protocol #Protocol# {
  SomeMethod(struct { req #Request#; });
};
)FIDL");
    TestLibrary library(source);
    ASSERT_COMPILED(library);
    auto decl_order = library.declaration_order();
    ASSERT_EQ(4u, decl_order.size());
    ASSERT_EQ(decl_order[0]->name.decl_name(), namer.of("Element"));
    ASSERT_EQ(decl_order[1]->name.decl_name(), namer.of("Request"));
    ASSERT_THAT(decl_order[2]->name.decl_name(), HasSubstr("ProtocolSomeMethodRequest"));
    ASSERT_EQ(decl_order[3]->name.decl_name(), namer.of("Protocol"));
  }
}

TEST(DeclarationOrderTests, GoodNullableRefBreaksDependency) {
  for (int i = 0; i < kRepeatTestCount; i++) {
    Namer namer;
    auto source = namer.mangle(R"FIDL(
library example;

type #Request# = resource struct {
  req array<box<#Element#>, 4>;
};

type #Element# = resource struct {
  prot client_end:#Protocol#;
};

protocol #Protocol# {
  SomeMethod(resource struct { req #Request#; });
};
)FIDL");
    TestLibrary library(source);
    ASSERT_COMPILED(library);
    auto decl_order = library.declaration_order();
    ASSERT_EQ(4u, decl_order.size());

    // Since the Element struct contains a Protocol handle, it does not
    // have any dependencies, and we therefore have two independent
    // declaration sub-graphs:
    //   a. Element
    //   b. Request <- ProtocolSomeMethodRequest <- Protocol
    // Because of random prefixes, either (a) or (b) will be selected to
    // be first in the declaration order.
    bool element_is_first = decl_order[0]->name.decl_name() == namer.of("Element");
    if (element_is_first) {
      ASSERT_EQ(decl_order[0]->name.decl_name(), namer.of("Element"));
      ASSERT_EQ(decl_order[1]->name.decl_name(), namer.of("Request"));
      ASSERT_THAT(decl_order[2]->name.decl_name(), HasSubstr("ProtocolSomeMethodRequest"));
      ASSERT_EQ(decl_order[3]->name.decl_name(), namer.of("Protocol"));
    } else {
      ASSERT_EQ(decl_order[0]->name.decl_name(), namer.of("Request"));
      ASSERT_THAT(decl_order[1]->name.decl_name(), HasSubstr("ProtocolSomeMethodRequest"));
      ASSERT_EQ(decl_order[2]->name.decl_name(), namer.of("Protocol"));
      ASSERT_EQ(decl_order[3]->name.decl_name(), namer.of("Element"));
    }
  }
}

TEST(DeclarationOrderTests, GoodRequestTypeBreaksDependencyGraph) {
  for (int i = 0; i < kRepeatTestCount; i++) {
    Namer namer;
    auto source = namer.mangle(R"FIDL(
library example;

type #Request# = resource struct {
  req server_end:#Protocol#;
};

protocol #Protocol# {
  SomeMethod(resource struct { req #Request#; });
};
)FIDL");
    TestLibrary library(source);
    ASSERT_COMPILED(library);
    auto decl_order = library.declaration_order();
    ASSERT_EQ(3u, decl_order.size());
    ASSERT_EQ(decl_order[0]->name.decl_name(), namer.of("Request"));
    ASSERT_THAT(decl_order[1]->name.decl_name(), HasSubstr("ProtocolSomeMethodRequest"));
    ASSERT_EQ(decl_order[2]->name.decl_name(), namer.of("Protocol"));
  }
}

TEST(DeclarationOrderTests, GoodNonnullableUnion) {
  for (int i = 0; i < kRepeatTestCount; i++) {
    Namer namer;
    auto source = namer.mangle(R"FIDL(
library example;

type #Union# = resource union {
  1: req server_end:#Protocol#;
  2: foo #Payload#;
};

protocol #Protocol# {
  SomeMethod(resource struct { req #Union#; });
};

type #Payload# = struct {
  a int32;
};
)FIDL");
    TestLibrary library(source);
    ASSERT_COMPILED(library);
    auto decl_order = library.declaration_order();
    ASSERT_EQ(4u, decl_order.size());
    ASSERT_EQ(decl_order[0]->name.decl_name(), namer.of("Payload"));
    ASSERT_EQ(decl_order[1]->name.decl_name(), namer.of("Union"));
    ASSERT_THAT(decl_order[2]->name.decl_name(), HasSubstr("ProtocolSomeMethodRequest"));
    ASSERT_EQ(decl_order[3]->name.decl_name(), namer.of("Protocol"));
  }
}

TEST(DeclarationOrderTests, GoodNullableUnion) {
  for (int i = 0; i < kRepeatTestCount; i++) {
    Namer namer;
    auto source = namer.mangle(R"FIDL(
library example;

type #Union# = resource union {
  1: req server_end:#Protocol#;
  2: foo #Payload#;
};

protocol #Protocol# {
  SomeMethod(resource struct { req #Union#:optional; });
};

type #Payload# = struct {
  a int32;
};
)FIDL");
    TestLibrary library(source);
    ASSERT_COMPILED(library);
    auto decl_order = library.declaration_order();
    ASSERT_EQ(4u, decl_order.size());

    // Since the Union argument is nullable, Protocol does not have any
    // dependencies, and we therefore have two independent declaration
    // sub-graphs:
    //   a. Payload <- Union
    //   b. ProtocolSomeMethodRequest <- Protocol
    // Because of random prefixes, either (a) or (b) will be selected to
    // be first in the declaration order.
    bool payload_is_first = decl_order[0]->name.decl_name() == namer.of("Payload");
    if (payload_is_first) {
      ASSERT_EQ(decl_order[0]->name.decl_name(), namer.of("Payload"));
      ASSERT_EQ(decl_order[1]->name.decl_name(), namer.of("Union"));
      ASSERT_THAT(decl_order[2]->name.decl_name(), HasSubstr("ProtocolSomeMethodRequest"));
      ASSERT_EQ(decl_order[3]->name.decl_name(), namer.of("Protocol"));
    } else {
      ASSERT_THAT(decl_order[0]->name.decl_name(), HasSubstr("ProtocolSomeMethodRequest"));
      ASSERT_EQ(decl_order[1]->name.decl_name(), namer.of("Protocol"));
      ASSERT_EQ(decl_order[2]->name.decl_name(), namer.of("Payload"));
      ASSERT_EQ(decl_order[3]->name.decl_name(), namer.of("Union"));
    }
  }
}

TEST(DeclarationOrderTests, GoodNonnullableUnionInStruct) {
  for (int i = 0; i < kRepeatTestCount; i++) {
    Namer namer;
    auto source = namer.mangle(R"FIDL(
library example;

type #Payload# = struct {
  a int32;
};

protocol #Protocol# {
  SomeMethod(struct { req #Request#; });
};

type #Request# = struct {
  u #Union#;
};

type #Union# = union {
  1: foo #Payload#;
};
)FIDL");
    TestLibrary library(source);
    ASSERT_COMPILED(library);
    auto decl_order = library.declaration_order();
    ASSERT_EQ(5u, decl_order.size());
    ASSERT_EQ(decl_order[0]->name.decl_name(), namer.of("Payload"));
    ASSERT_EQ(decl_order[1]->name.decl_name(), namer.of("Union"));
    ASSERT_EQ(decl_order[2]->name.decl_name(), namer.of("Request"));
    ASSERT_THAT(decl_order[3]->name.decl_name(), HasSubstr("ProtocolSomeMethodRequest"));
    ASSERT_EQ(decl_order[4]->name.decl_name(), namer.of("Protocol"));
  }
}

TEST(DeclarationOrderTests, GoodNullableUnionInStruct) {
  for (int i = 0; i < kRepeatTestCount; i++) {
    Namer namer;
    auto source = namer.mangle(R"FIDL(
library example;

type #Payload# = struct {
  a int32;
};

protocol #Protocol# {
  SomeMethod(struct { req #Request#; });
};

type #Request# = struct {
  u #Union#:optional;
};

type #Union# = union {
  1: foo #Payload#;
};
)FIDL");
    TestLibrary library(source);
    ASSERT_COMPILED(library);
    auto decl_order = library.declaration_order();
    ASSERT_EQ(5u, decl_order.size());

    // Since the Union field is nullable, Request does not have any
    // dependencies, and we therefore have two independent declaration
    // sub-graphs:
    //   a. Payload <- Union
    //   b. Request <- ProtocolSomeMethodRequest <- Protocol
    // Because of random prefixes, either (a) or (b) will be selected to
    // be first in the declaration order.
    bool payload_is_first = decl_order[0]->name.decl_name() == namer.of("Payload");
    if (payload_is_first) {
      ASSERT_EQ(decl_order[0]->name.decl_name(), namer.of("Payload"));
      ASSERT_EQ(decl_order[1]->name.decl_name(), namer.of("Union"));
      ASSERT_EQ(decl_order[2]->name.decl_name(), namer.of("Request"));
      ASSERT_THAT(decl_order[3]->name.decl_name(), HasSubstr("ProtocolSomeMethodRequest"));
      ASSERT_EQ(decl_order[4]->name.decl_name(), namer.of("Protocol"));
    } else {
      ASSERT_EQ(decl_order[0]->name.decl_name(), namer.of("Request"));
      ASSERT_THAT(decl_order[1]->name.decl_name(), HasSubstr("ProtocolSomeMethodRequest"));
      ASSERT_EQ(decl_order[2]->name.decl_name(), namer.of("Protocol"));
      ASSERT_EQ(decl_order[3]->name.decl_name(), namer.of("Payload"));
      ASSERT_EQ(decl_order[4]->name.decl_name(), namer.of("Union"));
    }
  }
}

TEST(DeclarationOrderTests, GoodMultipleLibraries) {
  for (int i = 0; i < kRepeatTestCount; i++) {
    SharedAmongstLibraries shared;
    TestLibrary dependency(&shared, "dependency.fidl", R"FIDL(
library dependency;

type ExampleDecl1 = struct {};
)FIDL");
    ASSERT_COMPILED(dependency);

    TestLibrary library(&shared, "example.fidl", R"FIDL(
library example;

using dependency;

type ExampleDecl0 = struct {};
type ExampleDecl2 = struct {};

protocol ExampleDecl1 {
  Method(struct { arg dependency.ExampleDecl1; });
};
)FIDL");
    ASSERT_COMPILED(library);

    auto dependency_decl_order = dependency.declaration_order();
    ASSERT_EQ(1u, dependency_decl_order.size());
    ASSERT_EQ(NameFlatName(dependency_decl_order[0]->name), "dependency/ExampleDecl1");

    auto library_decl_order = library.declaration_order();
    ASSERT_EQ(4u, library_decl_order.size());
    ASSERT_EQ(NameFlatName(library_decl_order[0]->name), "example/ExampleDecl2");
    ASSERT_EQ(NameFlatName(library_decl_order[1]->name), "example/ExampleDecl1MethodRequest");
    ASSERT_EQ(NameFlatName(library_decl_order[2]->name), "example/ExampleDecl1");
    ASSERT_EQ(NameFlatName(library_decl_order[3]->name), "example/ExampleDecl0");
  }
}

TEST(DeclarationOrderTests, GoodConstTypeComesFirst) {
  for (int i = 0; i < kRepeatTestCount; i++) {
    Namer namer;
    auto source = namer.mangle(R"FIDL(
library example;

const #Constant# #Alias# = 42;

alias #Alias# = uint32;
)FIDL");
    TestLibrary library(source);
    ASSERT_COMPILED(library);
    auto decl_order = library.declaration_order();
    ASSERT_EQ(2u, decl_order.size());
    ASSERT_EQ(decl_order[0]->name.decl_name(), namer.of("Alias"));
    ASSERT_EQ(decl_order[1]->name.decl_name(), namer.of("Constant"));
  }
}

TEST(DeclarationOrderTests, GoodEnumOrdinalTypeComesFirst) {
  for (int i = 0; i < kRepeatTestCount; i++) {
    Namer namer;
    auto source = namer.mangle(R"FIDL(
library example;

type #Enum# = enum : #Alias# { A = 1; };

alias #Alias# = uint32;
)FIDL");
    TestLibrary library(source);
    ASSERT_COMPILED(library);
    auto decl_order = library.declaration_order();
    ASSERT_EQ(2u, decl_order.size());
    ASSERT_EQ(decl_order[0]->name.decl_name(), namer.of("Alias"));
    ASSERT_EQ(decl_order[1]->name.decl_name(), namer.of("Enum"));
  }
}

TEST(DeclarationOrderTests, GoodBitsOrdinalTypeComesFirst) {
  for (int i = 0; i < kRepeatTestCount; i++) {
    Namer namer;
    auto source = namer.mangle(R"FIDL(
library example;

type #Bits# = bits : #Alias# { A = 1; };

alias #Alias# = uint32;
)FIDL");
    TestLibrary library(source);
    ASSERT_COMPILED(library);
    auto decl_order = library.declaration_order();
    ASSERT_EQ(2u, decl_order.size());
    ASSERT_EQ(decl_order[0]->name.decl_name(), namer.of("Alias"));
    ASSERT_EQ(decl_order[1]->name.decl_name(), namer.of("Bits"));
  }
}

}  // namespace
}  // namespace fidlc
