// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "tools/fidl/fidlc/include/fidl/diagnostics.h"
#include "tools/fidl/fidlc/tests/test_library.h"

// This file tests ways of interleaving the availability of a source element
// with that of a target element that it references. See also
// versioning_tests.cc and decomposition_tests.cc.

namespace {

using ::testing::Contains;

struct TestCase {
  // A code describing how to order the availabilities relative to each other,
  // using (a, d, r, l) for the source and (A, D, R, L) for the target:
  //
  //     source: @available(added=a, deprecated=d, removed=r/l, legacy=...)
  //     target: @available(added=A, deprecated=D, removed=R/L, legacy=...)
  //
  // For example, "AadrR" means: add target, add source, deprecate source,
  // remove source, remove target. Additionally, the character "=" is used to
  // align two values. For example, "a=A" means the source and target are added
  // at the same version, and never deprecated/removed.
  //
  // Using l/L instead of r/R means the element is removed with legacy=true.
  //
  // Must contain at least "a" and "A", but all others are optional.
  std::string_view code;

  // Expected errors. The order does not matter, and the list does not need to
  // be complete, because this file contains a large number of test cases and
  // stricter requirements would make it painful to update when errors change.
  std::vector<const fidlc::DiagnosticDef*> errors = {};

  struct Attributes {
    std::string source_available;
    std::string target_available;
  };

  // Generates the @available attributes for source and target.
  Attributes Format() const {
    std::stringstream source, target;
    source << "@available(";
    target << "@available(";
    int version = 1;
    for (auto c : code) {
      switch (c) {
        case 'a':
          source << "added=" << version;
          break;
        case 'd':
          source << ", deprecated=" << version;
          break;
        case 'r':
          source << ", removed=" << version;
          break;
        case 'l':
          source << ", removed=" << version << ", legacy = true";
          break;
        case 'A':
          target << "added=" << version;
          break;
        case 'D':
          target << ", deprecated=" << version;
          break;
        case 'R':
          target << ", removed=" << version;
          break;
        case 'L':
          target << ", removed=" << version << ", legacy=true";
          break;
        case '=':
          version -= 2;
          break;
      }
      ++version;
    }
    source << ')';
    target << ')';
    return Attributes{source.str(), target.str()};
  }

  // Compiles the library and asserts that it conforms to the test case.
  void CompileAndAssert(TestLibrary& library) const {
    if (errors.empty()) {
      ASSERT_COMPILED(library);
      return;
    }
    ASSERT_FALSE(library.Compile());
    std::set<std::string_view> actual_errors;
    for (auto& actual_error : library.errors()) {
      actual_errors.insert(actual_error->def.msg);
    }
    for (auto expected_error : errors) {
      EXPECT_THAT(actual_errors, Contains(expected_error->msg));
    }
  }
};

// These cases (except for some extras at the bottom) were generated with the
// following Python code:
//
//     def go(x, y):
//         if x is None or y is None:
//             return set()
//         if not (x or y):
//             return {""}
//         rest = lambda x: x[1:] if x else None
//         rx, ry, rxy = go(rest(x), y), go(x, rest(y)), go(rest(x), rest(y))
//         return {*rx, *ry, *rxy, *(x[0] + s for s in rx), *(y[0] + s for s in ry),
//                 *(f"{x[0]}={y[0]}{s}" for s in rxy)}
//
//     print("\n".join(sorted(s for s in go("adr", "ADR") if "a" in s and "A" in s)))
//
const TestCase kTestCases[] = {
    {"ADRa", {&fidlc::ErrNameNotFound}},
    {"ADRad", {&fidlc::ErrNameNotFound}},
    {"ADRadr", {&fidlc::ErrNameNotFound}},
    {"ADRar", {&fidlc::ErrNameNotFound}},
    {"ADa", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"ADa=R", {&fidlc::ErrNameNotFound}},
    {"ADa=Rd", {&fidlc::ErrNameNotFound}},
    {"ADa=Rdr", {&fidlc::ErrNameNotFound}},
    {"ADa=Rr", {&fidlc::ErrNameNotFound}},
    {"ADaR", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"ADaRd", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"ADaRdr", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"ADaRr", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"ADad", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"ADad=R", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"ADad=Rr", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"ADadR", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"ADadRr", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"ADadr", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"ADadr=R", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"ADadrR", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"ADar", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"ADar=R", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"ADarR", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"ARa", {&fidlc::ErrNameNotFound}},
    {"ARad", {&fidlc::ErrNameNotFound}},
    {"ARadr", {&fidlc::ErrNameNotFound}},
    {"ARar", {&fidlc::ErrNameNotFound}},
    {"Aa"},
    {"Aa=D", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"Aa=DR", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"Aa=DRd", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"Aa=DRdr", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"Aa=DRr", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"Aa=Dd", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"Aa=Dd=R", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"Aa=Dd=Rr", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"Aa=DdR", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"Aa=DdRr", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"Aa=Ddr", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"Aa=Ddr=R", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"Aa=DdrR", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"Aa=Dr", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"Aa=Dr=R", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"Aa=DrR", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"Aa=R", {&fidlc::ErrNameNotFound}},
    {"Aa=Rd", {&fidlc::ErrNameNotFound}},
    {"Aa=Rdr", {&fidlc::ErrNameNotFound}},
    {"Aa=Rr", {&fidlc::ErrNameNotFound}},
    {"AaD", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"AaDR", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"AaDRd", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"AaDRdr", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"AaDRr", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"AaDd", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"AaDd=R", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"AaDd=Rr", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"AaDdR", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"AaDdRr", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"AaDdr", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"AaDdr=R", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"AaDdrR", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"AaDr", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"AaDr=R", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"AaDrR", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"AaR", {&fidlc::ErrNameNotFound}},
    {"AaRd", {&fidlc::ErrNameNotFound}},
    {"AaRdr", {&fidlc::ErrNameNotFound}},
    {"AaRr", {&fidlc::ErrNameNotFound}},
    {"Aad"},
    {"Aad=D"},
    {"Aad=DR", {&fidlc::ErrNameNotFound}},
    {"Aad=DRr", {&fidlc::ErrNameNotFound}},
    {"Aad=Dr"},
    {"Aad=Dr=R"},
    {"Aad=DrR"},
    {"Aad=R", {&fidlc::ErrNameNotFound}},
    {"Aad=Rr", {&fidlc::ErrNameNotFound}},
    {"AadD"},
    {"AadDR", {&fidlc::ErrNameNotFound}},
    {"AadDRr", {&fidlc::ErrNameNotFound}},
    {"AadDr"},
    {"AadDr=R"},
    {"AadDrR"},
    {"AadR", {&fidlc::ErrNameNotFound}},
    {"AadRr", {&fidlc::ErrNameNotFound}},
    {"Aadr"},
    {"Aadr=D"},
    {"Aadr=DR"},
    {"Aadr=R"},
    {"AadrD"},
    {"AadrDR"},
    {"AadrR"},
    {"Aar"},
    {"Aar=D"},
    {"Aar=DR"},
    {"Aar=R"},
    {"AarD"},
    {"AarDR"},
    {"AarR"},
    {"a=A"},
    {"a=AD", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"a=ADR", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"a=ADRd", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"a=ADRdr", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"a=ADRr", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"a=ADd", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"a=ADd=R", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"a=ADd=Rr", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"a=ADdR", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"a=ADdRr", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"a=ADdr", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"a=ADdr=R", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"a=ADdrR", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"a=ADr", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"a=ADr=R", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"a=ADrR", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"a=AR", {&fidlc::ErrNameNotFound}},
    {"a=ARd", {&fidlc::ErrNameNotFound}},
    {"a=ARdr", {&fidlc::ErrNameNotFound}},
    {"a=ARr", {&fidlc::ErrNameNotFound}},
    {"a=Ad"},
    {"a=Ad=D"},
    {"a=Ad=DR", {&fidlc::ErrNameNotFound}},
    {"a=Ad=DRr", {&fidlc::ErrNameNotFound}},
    {"a=Ad=Dr"},
    {"a=Ad=Dr=R"},
    {"a=Ad=DrR"},
    {"a=Ad=R", {&fidlc::ErrNameNotFound}},
    {"a=Ad=Rr", {&fidlc::ErrNameNotFound}},
    {"a=AdD"},
    {"a=AdDR", {&fidlc::ErrNameNotFound}},
    {"a=AdDRr", {&fidlc::ErrNameNotFound}},
    {"a=AdDr"},
    {"a=AdDr=R"},
    {"a=AdDrR"},
    {"a=AdR", {&fidlc::ErrNameNotFound}},
    {"a=AdRr", {&fidlc::ErrNameNotFound}},
    {"a=Adr"},
    {"a=Adr=D"},
    {"a=Adr=DR"},
    {"a=Adr=R"},
    {"a=AdrD"},
    {"a=AdrDR"},
    {"a=AdrR"},
    {"a=Ar"},
    {"a=Ar=D"},
    {"a=Ar=DR"},
    {"a=Ar=R"},
    {"a=ArD"},
    {"a=ArDR"},
    {"a=ArR"},
    {"aA", {&fidlc::ErrNameNotFound}},
    {"aAD", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"aADR", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"aADRd", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"aADRdr", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"aADRr", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"aADd", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"aADd=R", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"aADd=Rr", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"aADdR", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"aADdRr", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"aADdr", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"aADdr=R", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"aADdrR", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"aADr", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"aADr=R", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"aADrR", {&fidlc::ErrInvalidReferenceToDeprecated, &fidlc::ErrNameNotFound}},
    {"aAR", {&fidlc::ErrNameNotFound}},
    {"aARd", {&fidlc::ErrNameNotFound}},
    {"aARdr", {&fidlc::ErrNameNotFound}},
    {"aARr", {&fidlc::ErrNameNotFound}},
    {"aAd", {&fidlc::ErrNameNotFound}},
    {"aAd=D", {&fidlc::ErrNameNotFound}},
    {"aAd=DR", {&fidlc::ErrNameNotFound}},
    {"aAd=DRr", {&fidlc::ErrNameNotFound}},
    {"aAd=Dr", {&fidlc::ErrNameNotFound}},
    {"aAd=Dr=R", {&fidlc::ErrNameNotFound}},
    {"aAd=DrR", {&fidlc::ErrNameNotFound}},
    {"aAd=R", {&fidlc::ErrNameNotFound}},
    {"aAd=Rr", {&fidlc::ErrNameNotFound}},
    {"aAdD", {&fidlc::ErrNameNotFound}},
    {"aAdDR", {&fidlc::ErrNameNotFound}},
    {"aAdDRr", {&fidlc::ErrNameNotFound}},
    {"aAdDr", {&fidlc::ErrNameNotFound}},
    {"aAdDr=R", {&fidlc::ErrNameNotFound}},
    {"aAdDrR", {&fidlc::ErrNameNotFound}},
    {"aAdR", {&fidlc::ErrNameNotFound}},
    {"aAdRr", {&fidlc::ErrNameNotFound}},
    {"aAdr", {&fidlc::ErrNameNotFound}},
    {"aAdr=D", {&fidlc::ErrNameNotFound}},
    {"aAdr=DR", {&fidlc::ErrNameNotFound}},
    {"aAdr=R", {&fidlc::ErrNameNotFound}},
    {"aAdrD", {&fidlc::ErrNameNotFound}},
    {"aAdrDR", {&fidlc::ErrNameNotFound}},
    {"aAdrR", {&fidlc::ErrNameNotFound}},
    {"aAr", {&fidlc::ErrNameNotFound}},
    {"aAr=D", {&fidlc::ErrNameNotFound}},
    {"aAr=DR", {&fidlc::ErrNameNotFound}},
    {"aAr=R", {&fidlc::ErrNameNotFound}},
    {"aArD", {&fidlc::ErrNameNotFound}},
    {"aArDR", {&fidlc::ErrNameNotFound}},
    {"aArR", {&fidlc::ErrNameNotFound}},
    {"ad=A", {&fidlc::ErrNameNotFound}},
    {"ad=AD", {&fidlc::ErrNameNotFound}},
    {"ad=ADR", {&fidlc::ErrNameNotFound}},
    {"ad=ADRr", {&fidlc::ErrNameNotFound}},
    {"ad=ADr", {&fidlc::ErrNameNotFound}},
    {"ad=ADr=R", {&fidlc::ErrNameNotFound}},
    {"ad=ADrR", {&fidlc::ErrNameNotFound}},
    {"ad=AR", {&fidlc::ErrNameNotFound}},
    {"ad=ARr", {&fidlc::ErrNameNotFound}},
    {"ad=Ar", {&fidlc::ErrNameNotFound}},
    {"ad=Ar=D", {&fidlc::ErrNameNotFound}},
    {"ad=Ar=DR", {&fidlc::ErrNameNotFound}},
    {"ad=Ar=R", {&fidlc::ErrNameNotFound}},
    {"ad=ArD", {&fidlc::ErrNameNotFound}},
    {"ad=ArDR", {&fidlc::ErrNameNotFound}},
    {"ad=ArR", {&fidlc::ErrNameNotFound}},
    {"adA", {&fidlc::ErrNameNotFound}},
    {"adAD", {&fidlc::ErrNameNotFound}},
    {"adADR", {&fidlc::ErrNameNotFound}},
    {"adADRr", {&fidlc::ErrNameNotFound}},
    {"adADr", {&fidlc::ErrNameNotFound}},
    {"adADr=R", {&fidlc::ErrNameNotFound}},
    {"adADrR", {&fidlc::ErrNameNotFound}},
    {"adAR", {&fidlc::ErrNameNotFound}},
    {"adARr", {&fidlc::ErrNameNotFound}},
    {"adAr", {&fidlc::ErrNameNotFound}},
    {"adAr=D", {&fidlc::ErrNameNotFound}},
    {"adAr=DR", {&fidlc::ErrNameNotFound}},
    {"adAr=R", {&fidlc::ErrNameNotFound}},
    {"adArD", {&fidlc::ErrNameNotFound}},
    {"adArDR", {&fidlc::ErrNameNotFound}},
    {"adArR", {&fidlc::ErrNameNotFound}},
    {"adr=A", {&fidlc::ErrNameNotFound}},
    {"adr=AD", {&fidlc::ErrNameNotFound}},
    {"adr=ADR", {&fidlc::ErrNameNotFound}},
    {"adr=AR", {&fidlc::ErrNameNotFound}},
    {"adrA", {&fidlc::ErrNameNotFound}},
    {"adrAD", {&fidlc::ErrNameNotFound}},
    {"adrADR", {&fidlc::ErrNameNotFound}},
    {"adrAR", {&fidlc::ErrNameNotFound}},
    {"ar=A", {&fidlc::ErrNameNotFound}},
    {"ar=AD", {&fidlc::ErrNameNotFound}},
    {"ar=ADR", {&fidlc::ErrNameNotFound}},
    {"ar=AR", {&fidlc::ErrNameNotFound}},
    {"arA", {&fidlc::ErrNameNotFound}},
    {"arAD", {&fidlc::ErrNameNotFound}},
    {"arADR", {&fidlc::ErrNameNotFound}},
    {"arAR", {&fidlc::ErrNameNotFound}},

    // Some manual cases for LEGACY. Doing all permutations would grow the list
    // above from 252 to 730 entries.
    {"AadDlL"},
    {"AadlD"},
    {"AalD", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"AalDL", {&fidlc::ErrInvalidReferenceToDeprecated}},
    {"AalDR", {&fidlc::ErrNameNotFound}},
    {"AalL"},
    {"a=AL", {&fidlc::ErrNameNotFound}},
    {"a=Ad=Dl=L"},
    {"a=Al"},
    {"a=Al=L"},
    {"a=Al=R", {&fidlc::ErrNameNotFound}},
    {"a=Ar=L"},
    {"alAL", {&fidlc::ErrNameNotFound}},
};

// Substitutes replacement for placeholder in str.
void substitute(std::string& str, std::string_view placeholder, std::string_view replacement) {
  str.replace(str.find(placeholder), placeholder.size(), replacement);
}

TEST(AvailabilityInterleavingTests, SameLibrary) {
  for (auto& test_case : kTestCases) {
    auto attributes = test_case.Format();
    std::string fidl = R"FIDL(
@available(added=1)
library example;

${source_available}
const SOURCE bool = TARGET;

${target_available}
const TARGET bool = false;
)FIDL";
    substitute(fidl, "${source_available}", attributes.source_available);
    substitute(fidl, "${target_available}", attributes.target_available);
    SCOPED_TRACE(testing::Message() << "code: " << test_case.code << ", fidl:\n\n" << fidl);
    TestLibrary library(fidl);
    library.SelectVersion("example", "HEAD");
    test_case.CompileAndAssert(library);
  }
}

// Tests compilation of example_fidl and dependency_fidl after substituting
// ${source_available} in example_fidl and ${target_available} in
// dependency_fidl using the values from test_case.
void TestExternalLibrary(const TestCase& test_case, std::string example_fidl,
                         std::string dependency_fidl) {
  SharedAmongstLibraries shared;
  shared.SelectVersion("platform", "HEAD");
  auto attributes = test_case.Format();
  substitute(dependency_fidl, "${target_available}", attributes.target_available);
  substitute(example_fidl, "${source_available}", attributes.source_available);
  SCOPED_TRACE(testing::Message() << "code: " << test_case.code << ", dependency.fidl:\n\n"
                                  << dependency_fidl << "\n\nexample.fidl:\n\n"
                                  << example_fidl);
  TestLibrary dependency(&shared, "dependency.fidl", dependency_fidl);
  ASSERT_COMPILED(dependency);
  TestLibrary example(&shared, "example.fidl", example_fidl);
  test_case.CompileAndAssert(example);
}

TEST(AvailabilityInterleavingTests, DeclToDeclExternal) {
  std::string example_fidl = R"FIDL(
@available(added=1)
library platform.example;

using platform.dependency;

${source_available}
const SOURCE bool = platform.dependency.TARGET;
)FIDL";
  std::string dependency_fidl = R"FIDL(
@available(added=1)
library platform.dependency;

${target_available}
const TARGET bool = false;
)FIDL";
  for (auto& test_case : kTestCases) {
    TestExternalLibrary(test_case, example_fidl, dependency_fidl);
  }
}

TEST(AvailabilityInterleavingTests, LibraryToLibraryExternal) {
  std::string example_fidl = R"FIDL(
${source_available}
library platform.example;

using platform.dependency;

const SOURCE bool = platform.dependency.TARGET;
)FIDL";
  std::string dependency_fidl = R"FIDL(
${target_available}
library platform.dependency;

const TARGET bool = false;
)FIDL";
  for (auto& test_case : kTestCases) {
    TestExternalLibrary(test_case, example_fidl, dependency_fidl);
  }
}

TEST(AvailabilityInterleavingTests, LibraryToDeclExternal) {
  std::string example_fidl = R"FIDL(
${source_available}
library platform.example;

using platform.dependency;

const SOURCE bool = platform.dependency.TARGET;
)FIDL";
  std::string dependency_fidl = R"FIDL(
@available(added=1)
library platform.dependency;

${target_available}
const TARGET bool = false;
)FIDL";
  for (auto& test_case : kTestCases) {
    TestExternalLibrary(test_case, example_fidl, dependency_fidl);
  }
}

TEST(AvailabilityInterleavingTests, DeclToLibraryExternal) {
  std::string example_fidl = R"FIDL(
@available(added=1)
library platform.example;

using platform.dependency;

${source_available}
const SOURCE bool = platform.dependency.TARGET;
)FIDL";
  std::string dependency_fidl = R"FIDL(
${target_available}
library platform.dependency;

const TARGET bool = false;
)FIDL";
  for (auto& test_case : kTestCases) {
    TestExternalLibrary(test_case, example_fidl, dependency_fidl);
  }
}

TEST(AvailabilityInterleavingTests, Error0055) {
  TestLibrary library;
  library.AddFile("bad/fi-0055.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(fidlc::ErrInvalidReferenceToDeprecated, "alias 'RGB'",
                     fidlc::VersionRange(fidlc::Version::From(3).value(), fidlc::Version::PosInf()),
                     fidlc::Platform::Parse("test").value(), "table member 'color'");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(AvailabilityInterleavingTests, Error0056) {
  SharedAmongstLibraries shared;
  shared.SelectVersion("foo", "HEAD");
  shared.SelectVersion("bar", "HEAD");
  TestLibrary dependency(&shared);
  dependency.AddFile("bad/fi-0056-a.test.fidl");
  ASSERT_COMPILED(dependency);
  TestLibrary library(&shared);
  library.AddFile("bad/fi-0056-b.test.fidl");
  library.ExpectFail(fidlc::ErrInvalidReferenceToDeprecatedOtherPlatform, "alias 'RGB'",
                     fidlc::VersionRange(fidlc::Version::From(2).value(), fidlc::Version::PosInf()),
                     fidlc::Platform::Parse("foo").value(), "table member 'color'",
                     fidlc::VersionRange(fidlc::Version::From(3).value(), fidlc::Version::PosInf()),
                     fidlc::Platform::Parse("bar").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

}  // namespace
