// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "tools/fidl/fidlc/src/diagnostics.h"
#include "tools/fidl/fidlc/tests/test_library.h"

// This file tests ways of interleaving the availability of a source element
// with that of a target element that it references.

namespace fidlc {
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
  std::vector<const DiagnosticDef*> errors = {};

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
    {"ADRa", {&ErrNameNotFound}},
    {"ADRad", {&ErrNameNotFound}},
    {"ADRadr", {&ErrNameNotFound}},
    {"ADRar", {&ErrNameNotFound}},
    {"ADa", {&ErrInvalidReferenceToDeprecated}},
    {"ADa=R", {&ErrNameNotFound}},
    {"ADa=Rd", {&ErrNameNotFound}},
    {"ADa=Rdr", {&ErrNameNotFound}},
    {"ADa=Rr", {&ErrNameNotFound}},
    {"ADaR", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"ADaRd", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"ADaRdr", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"ADaRr", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"ADad", {&ErrInvalidReferenceToDeprecated}},
    {"ADad=R", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"ADad=Rr", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"ADadR", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"ADadRr", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"ADadr", {&ErrInvalidReferenceToDeprecated}},
    {"ADadr=R", {&ErrInvalidReferenceToDeprecated}},
    {"ADadrR", {&ErrInvalidReferenceToDeprecated}},
    {"ADar", {&ErrInvalidReferenceToDeprecated}},
    {"ADar=R", {&ErrInvalidReferenceToDeprecated}},
    {"ADarR", {&ErrInvalidReferenceToDeprecated}},
    {"ARa", {&ErrNameNotFound}},
    {"ARad", {&ErrNameNotFound}},
    {"ARadr", {&ErrNameNotFound}},
    {"ARar", {&ErrNameNotFound}},
    {"Aa"},
    {"Aa=D", {&ErrInvalidReferenceToDeprecated}},
    {"Aa=DR", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"Aa=DRd", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"Aa=DRdr", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"Aa=DRr", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"Aa=Dd", {&ErrInvalidReferenceToDeprecated}},
    {"Aa=Dd=R", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"Aa=Dd=Rr", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"Aa=DdR", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"Aa=DdRr", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"Aa=Ddr", {&ErrInvalidReferenceToDeprecated}},
    {"Aa=Ddr=R", {&ErrInvalidReferenceToDeprecated}},
    {"Aa=DdrR", {&ErrInvalidReferenceToDeprecated}},
    {"Aa=Dr", {&ErrInvalidReferenceToDeprecated}},
    {"Aa=Dr=R", {&ErrInvalidReferenceToDeprecated}},
    {"Aa=DrR", {&ErrInvalidReferenceToDeprecated}},
    {"Aa=R", {&ErrNameNotFound}},
    {"Aa=Rd", {&ErrNameNotFound}},
    {"Aa=Rdr", {&ErrNameNotFound}},
    {"Aa=Rr", {&ErrNameNotFound}},
    {"AaD", {&ErrInvalidReferenceToDeprecated}},
    {"AaDR", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"AaDRd", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"AaDRdr", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"AaDRr", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"AaDd", {&ErrInvalidReferenceToDeprecated}},
    {"AaDd=R", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"AaDd=Rr", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"AaDdR", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"AaDdRr", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"AaDdr", {&ErrInvalidReferenceToDeprecated}},
    {"AaDdr=R", {&ErrInvalidReferenceToDeprecated}},
    {"AaDdrR", {&ErrInvalidReferenceToDeprecated}},
    {"AaDr", {&ErrInvalidReferenceToDeprecated}},
    {"AaDr=R", {&ErrInvalidReferenceToDeprecated}},
    {"AaDrR", {&ErrInvalidReferenceToDeprecated}},
    {"AaR", {&ErrNameNotFound}},
    {"AaRd", {&ErrNameNotFound}},
    {"AaRdr", {&ErrNameNotFound}},
    {"AaRr", {&ErrNameNotFound}},
    {"Aad"},
    {"Aad=D"},
    {"Aad=DR", {&ErrNameNotFound}},
    {"Aad=DRr", {&ErrNameNotFound}},
    {"Aad=Dr"},
    {"Aad=Dr=R"},
    {"Aad=DrR"},
    {"Aad=R", {&ErrNameNotFound}},
    {"Aad=Rr", {&ErrNameNotFound}},
    {"AadD"},
    {"AadDR", {&ErrNameNotFound}},
    {"AadDRr", {&ErrNameNotFound}},
    {"AadDr"},
    {"AadDr=R"},
    {"AadDrR"},
    {"AadR", {&ErrNameNotFound}},
    {"AadRr", {&ErrNameNotFound}},
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
    {"a=AD", {&ErrInvalidReferenceToDeprecated}},
    {"a=ADR", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"a=ADRd", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"a=ADRdr", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"a=ADRr", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"a=ADd", {&ErrInvalidReferenceToDeprecated}},
    {"a=ADd=R", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"a=ADd=Rr", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"a=ADdR", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"a=ADdRr", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"a=ADdr", {&ErrInvalidReferenceToDeprecated}},
    {"a=ADdr=R", {&ErrInvalidReferenceToDeprecated}},
    {"a=ADdrR", {&ErrInvalidReferenceToDeprecated}},
    {"a=ADr", {&ErrInvalidReferenceToDeprecated}},
    {"a=ADr=R", {&ErrInvalidReferenceToDeprecated}},
    {"a=ADrR", {&ErrInvalidReferenceToDeprecated}},
    {"a=AR", {&ErrNameNotFound}},
    {"a=ARd", {&ErrNameNotFound}},
    {"a=ARdr", {&ErrNameNotFound}},
    {"a=ARr", {&ErrNameNotFound}},
    {"a=Ad"},
    {"a=Ad=D"},
    {"a=Ad=DR", {&ErrNameNotFound}},
    {"a=Ad=DRr", {&ErrNameNotFound}},
    {"a=Ad=Dr"},
    {"a=Ad=Dr=R"},
    {"a=Ad=DrR"},
    {"a=Ad=R", {&ErrNameNotFound}},
    {"a=Ad=Rr", {&ErrNameNotFound}},
    {"a=AdD"},
    {"a=AdDR", {&ErrNameNotFound}},
    {"a=AdDRr", {&ErrNameNotFound}},
    {"a=AdDr"},
    {"a=AdDr=R"},
    {"a=AdDrR"},
    {"a=AdR", {&ErrNameNotFound}},
    {"a=AdRr", {&ErrNameNotFound}},
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
    {"aA", {&ErrNameNotFound}},
    {"aAD", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"aADR", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"aADRd", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"aADRdr", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"aADRr", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"aADd", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"aADd=R", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"aADd=Rr", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"aADdR", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"aADdRr", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"aADdr", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"aADdr=R", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"aADdrR", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"aADr", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"aADr=R", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"aADrR", {&ErrInvalidReferenceToDeprecated, &ErrNameNotFound}},
    {"aAR", {&ErrNameNotFound}},
    {"aARd", {&ErrNameNotFound}},
    {"aARdr", {&ErrNameNotFound}},
    {"aARr", {&ErrNameNotFound}},
    {"aAd", {&ErrNameNotFound}},
    {"aAd=D", {&ErrNameNotFound}},
    {"aAd=DR", {&ErrNameNotFound}},
    {"aAd=DRr", {&ErrNameNotFound}},
    {"aAd=Dr", {&ErrNameNotFound}},
    {"aAd=Dr=R", {&ErrNameNotFound}},
    {"aAd=DrR", {&ErrNameNotFound}},
    {"aAd=R", {&ErrNameNotFound}},
    {"aAd=Rr", {&ErrNameNotFound}},
    {"aAdD", {&ErrNameNotFound}},
    {"aAdDR", {&ErrNameNotFound}},
    {"aAdDRr", {&ErrNameNotFound}},
    {"aAdDr", {&ErrNameNotFound}},
    {"aAdDr=R", {&ErrNameNotFound}},
    {"aAdDrR", {&ErrNameNotFound}},
    {"aAdR", {&ErrNameNotFound}},
    {"aAdRr", {&ErrNameNotFound}},
    {"aAdr", {&ErrNameNotFound}},
    {"aAdr=D", {&ErrNameNotFound}},
    {"aAdr=DR", {&ErrNameNotFound}},
    {"aAdr=R", {&ErrNameNotFound}},
    {"aAdrD", {&ErrNameNotFound}},
    {"aAdrDR", {&ErrNameNotFound}},
    {"aAdrR", {&ErrNameNotFound}},
    {"aAr", {&ErrNameNotFound}},
    {"aAr=D", {&ErrNameNotFound}},
    {"aAr=DR", {&ErrNameNotFound}},
    {"aAr=R", {&ErrNameNotFound}},
    {"aArD", {&ErrNameNotFound}},
    {"aArDR", {&ErrNameNotFound}},
    {"aArR", {&ErrNameNotFound}},
    {"ad=A", {&ErrNameNotFound}},
    {"ad=AD", {&ErrNameNotFound}},
    {"ad=ADR", {&ErrNameNotFound}},
    {"ad=ADRr", {&ErrNameNotFound}},
    {"ad=ADr", {&ErrNameNotFound}},
    {"ad=ADr=R", {&ErrNameNotFound}},
    {"ad=ADrR", {&ErrNameNotFound}},
    {"ad=AR", {&ErrNameNotFound}},
    {"ad=ARr", {&ErrNameNotFound}},
    {"ad=Ar", {&ErrNameNotFound}},
    {"ad=Ar=D", {&ErrNameNotFound}},
    {"ad=Ar=DR", {&ErrNameNotFound}},
    {"ad=Ar=R", {&ErrNameNotFound}},
    {"ad=ArD", {&ErrNameNotFound}},
    {"ad=ArDR", {&ErrNameNotFound}},
    {"ad=ArR", {&ErrNameNotFound}},
    {"adA", {&ErrNameNotFound}},
    {"adAD", {&ErrNameNotFound}},
    {"adADR", {&ErrNameNotFound}},
    {"adADRr", {&ErrNameNotFound}},
    {"adADr", {&ErrNameNotFound}},
    {"adADr=R", {&ErrNameNotFound}},
    {"adADrR", {&ErrNameNotFound}},
    {"adAR", {&ErrNameNotFound}},
    {"adARr", {&ErrNameNotFound}},
    {"adAr", {&ErrNameNotFound}},
    {"adAr=D", {&ErrNameNotFound}},
    {"adAr=DR", {&ErrNameNotFound}},
    {"adAr=R", {&ErrNameNotFound}},
    {"adArD", {&ErrNameNotFound}},
    {"adArDR", {&ErrNameNotFound}},
    {"adArR", {&ErrNameNotFound}},
    {"adr=A", {&ErrNameNotFound}},
    {"adr=AD", {&ErrNameNotFound}},
    {"adr=ADR", {&ErrNameNotFound}},
    {"adr=AR", {&ErrNameNotFound}},
    {"adrA", {&ErrNameNotFound}},
    {"adrAD", {&ErrNameNotFound}},
    {"adrADR", {&ErrNameNotFound}},
    {"adrAR", {&ErrNameNotFound}},
    {"ar=A", {&ErrNameNotFound}},
    {"ar=AD", {&ErrNameNotFound}},
    {"ar=ADR", {&ErrNameNotFound}},
    {"ar=AR", {&ErrNameNotFound}},
    {"arA", {&ErrNameNotFound}},
    {"arAD", {&ErrNameNotFound}},
    {"arADR", {&ErrNameNotFound}},
    {"arAR", {&ErrNameNotFound}},

    // Some manual cases for LEGACY. Doing all permutations would grow the list
    // above from 252 to 730 entries.
    {"AadDlL"},
    {"AadlD"},
    {"AalD", {&ErrInvalidReferenceToDeprecated}},
    {"AalDL", {&ErrInvalidReferenceToDeprecated}},
    {"AalDR", {&ErrNameNotFound}},
    {"AalL"},
    {"a=AL", {&ErrNameNotFound}},
    {"a=Ad=Dl=L"},
    {"a=Al"},
    {"a=Al=L"},
    {"a=Al=R", {&ErrNameNotFound}},
    {"a=Ar=L"},
    {"alAL", {&ErrNameNotFound}},
};

// Substitutes replacement for placeholder in str.
void substitute(std::string& str, std::string_view placeholder, std::string_view replacement) {
  str.replace(str.find(placeholder), placeholder.size(), replacement);
}

TEST(VersioningInterleavingTests, SameLibrary) {
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

TEST(VersioningInterleavingTests, DeclToDeclExternal) {
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

TEST(VersioningInterleavingTests, LibraryToLibraryExternal) {
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

TEST(VersioningInterleavingTests, LibraryToDeclExternal) {
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

TEST(VersioningInterleavingTests, DeclToLibraryExternal) {
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

TEST(VersioningInterleavingTests, Error0055) {
  TestLibrary library;
  library.AddFile("bad/fi-0055.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(ErrInvalidReferenceToDeprecated, "alias 'RGB'",
                     VersionRange(Version::From(3).value(), Version::kPosInf),
                     Platform::Parse("test").value(), "table member 'color'");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningInterleavingTests, Error0056) {
  SharedAmongstLibraries shared;
  shared.SelectVersion("foo", "HEAD");
  shared.SelectVersion("bar", "HEAD");
  TestLibrary dependency(&shared);
  dependency.AddFile("bad/fi-0056-a.test.fidl");
  ASSERT_COMPILED(dependency);
  TestLibrary library(&shared);
  library.AddFile("bad/fi-0056-b.test.fidl");
  library.ExpectFail(ErrInvalidReferenceToDeprecatedOtherPlatform, "alias 'RGB'",
                     VersionRange(Version::From(2).value(), Version::kPosInf),
                     Platform::Parse("foo").value(), "table member 'color'",
                     VersionRange(Version::From(3).value(), Version::kPosInf),
                     Platform::Parse("bar").value());
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

}  // namespace
}  // namespace fidlc
