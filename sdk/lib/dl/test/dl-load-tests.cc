// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <format>
#include <thread>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "dl-impl-tests.h"
#include "dl-system-tests.h"
#include "startup-symbols.h"

// It's too much hassle the generate ELF test modules on a system where the
// host code is not usually built with ELF, so don't bother trying to test any
// of the ELF-loading logic on such hosts.  Unfortunately this means not
// discovering any <dlfcn.h> API differences from another non-ELF system that
// has that API, such as macOS.
#ifndef __ELF__
#error "This file should not be used on non-ELF hosts."
#endif

namespace {

// These are a convenience functions to specify that a specific dependency
// should or should not be found in the Needed set.
constexpr std::pair<std::string_view, bool> Found(std::string_view name) { return {name, true}; }

constexpr std::pair<std::string_view, bool> NotFound(std::string_view name) {
  return {name, false};
}

// Helper functions that will suffix strings with the current test name.
std::string TestSym(std::string_view symbol) {
  const testing::TestInfo* const test_info =
      ::testing::UnitTest::GetInstance()->current_test_info();
  return std::string{symbol} + "_" + test_info->name();
}

std::string TestModule(std::string_view symbol) {
  const testing::TestInfo* const test_info =
      ::testing::UnitTest::GetInstance()->current_test_info();
  return std::string{symbol} + "." + test_info->name() + ".module.so";
}

std::string TestShlib(std::string_view symbol) {
  const testing::TestInfo* const test_info =
      ::testing::UnitTest::GetInstance()->current_test_info();
  return std::string{symbol} + "." + test_info->name() + ".so";
}

// Cast `symbol` into a function returning type T and run it.
template <typename T>
T RunFunction(void* symbol __attribute__((nonnull))) {
  auto func_ptr = reinterpret_cast<T (*)()>(reinterpret_cast<uintptr_t>(symbol));
  return func_ptr();
}

using ::testing::MatchesRegex;

template <class Fixture>
using DlTests = Fixture;

// A matcher that uses MatchesRegex object that match the format of the error
// messages for dlopen() and dlsym() when a symbol is undefined.
// Example:
//    EXPECT_THAT(msg, IsUndefinedSymbolErrMsg(name, module));
MATCHER_P2(IsUndefinedSymbolErrMsg, symbol_name, module_name,
           std::format("error for undefined symbol {} in module {}", symbol_name, module_name)) {
  return testing::ExplainMatchResult(
      MatchesRegex(std::format(
          // Emitted by Fuchsia-musl when dlsym fails to locate the symbol.
          "Symbol not found: {}"
          // Emitted by Fuchsia-musl when relocation fails to resolve the symbol.
          "|.*Error relocating {}: {}: symbol not found"
          // Emitted by Linux-glibc and Libdl.
          "|.*{}: undefined symbol: {}",
          symbol_name, module_name, symbol_name, module_name, symbol_name)),
      arg, result_listener);
}

// This lists the test fixture classes to run DlTests tests against. The
// DlImplTests fixture is a framework for testing the implementation in
// libdl and the DlSystemTests fixture proxies to the system-provided dynamic
// linker. These tests ensure that both dynamic linker implementations meet
// expectations and behave the same way, with exceptions noted within the test.
using TestTypes = ::testing::Types<
#ifdef __Fuchsia__
    dl::testing::DlImplLoadZirconTests,
#endif
// TODO(https://fxbug.dev/324650368): Test fixtures currently retrieve files
// from different prefixed locations depending on the platform. Find a way
// to use a singular API to return the prefixed path specific to the platform so
// that the TestPosix fixture can run on Fuchsia as well.
#ifndef __Fuchsia__
    // libdl's POSIX test fixture can also be tested on Fuchsia and is included
    // for any ELF supported host.
    dl::testing::DlImplLoadPosixTests,
#endif
    dl::testing::DlSystemTests>;

TYPED_TEST_SUITE(DlTests, TestTypes);

TYPED_TEST(DlTests, NotFound) {
  const std::string kFile = TestModule("does-not-exist");

  this->ExpectMissing(kFile);

  auto result = this->DlOpen(kFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_error());
  if constexpr (TestFixture::kCanMatchExactError) {
    EXPECT_EQ(result.error_value().take_str(), "does-not-exist.NotFound.module.so not found");
  } else {
    EXPECT_THAT(
        result.error_value().take_str(),
        MatchesRegex(
            // emitted by Fuchsia-musl
            "Error loading shared library .*does-not-exist.NotFound.module.so: ZX_ERR_NOT_FOUND"
            // emitted by Linux-glibc
            "|.*does-not-exist.NotFound.module.so: cannot open shared object file: No such file or directory"));
  }
}

TYPED_TEST(DlTests, InvalidMode) {
  const std::string kFile = TestModule("ret17");

  if constexpr (!TestFixture::kCanValidateMode) {
    GTEST_SKIP() << "test requires dlopen to validate mode argment";
  }

  int bad_mode = -1;
  // The sanitizer runtimes (on non-Fuchsia hosts) intercept dlopen calls with
  // RTLD_DEEPBIND and make them fail without really calling -ldl's dlopen to
  // see if it would fail anyway.  So avoid having that flag set in the bad
  // mode argument.
#ifdef RTLD_DEEPBIND
  bad_mode &= ~RTLD_DEEPBIND;
#endif
  // Make sure the bad_mode does not produce a false positive with RTLD_NOLOAD
  // checks by the test fixture.
  bad_mode &= ~RTLD_NOLOAD;

  auto result = this->DlOpen(kFile.c_str(), bad_mode);
  ASSERT_TRUE(result.is_error());
  EXPECT_EQ(result.error_value().take_str(), "invalid mode parameter")
      << "for mode argument " << bad_mode;
}

// Load a basic file with no dependencies.
TYPED_TEST(DlTests, Basic) {
  constexpr int64_t kReturnValue = 17;
  const std::string kFile = TestModule("ret17");

  this->ExpectRootModule(kFile);

  auto result = this->DlOpen(kFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_ok()) << result.error_value();
  EXPECT_TRUE(result.value());

  // Look up the TestSym("TestStart").c_str() function and call it, expecting it to return 17.
  auto sym_result = this->DlSym(result.value(), TestSym("TestStart").c_str());
  ASSERT_TRUE(sym_result.is_ok()) << result.error_value();
  ASSERT_TRUE(sym_result.value());

  EXPECT_EQ(RunFunction<int64_t>(sym_result.value()), kReturnValue);

  ASSERT_TRUE(this->DlClose(result.value()).is_ok());
}

// Load a file that performs relative relocations against itself. The TestStart
// function's return value is derived from the resolved symbols.
TYPED_TEST(DlTests, Relative) {
  constexpr int64_t kReturnValue = 17;
  const std::string kFile = TestModule("relative-reloc");

  this->ExpectRootModule(kFile);

  auto result = this->DlOpen(kFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_ok()) << result.error_value();
  EXPECT_TRUE(result.value());

  auto sym_result = this->DlSym(result.value(), TestSym("TestStart").c_str());
  ASSERT_TRUE(sym_result.is_ok()) << result.error_value();
  ASSERT_TRUE(sym_result.value());

  EXPECT_EQ(RunFunction<int64_t>(sym_result.value()), kReturnValue);

  ASSERT_TRUE(this->DlClose(result.value()).is_ok());
}

// Load a file that performs symbolic relocations against itself. The TestStart
// functions' return value is derived from the resolved symbols.
TYPED_TEST(DlTests, Symbolic) {
  constexpr int64_t kReturnValue = 17;
  const std::string kFile = TestModule("symbolic-reloc");

  this->ExpectRootModule(kFile);

  auto result = this->DlOpen(kFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_ok()) << result.error_value();
  EXPECT_TRUE(result.value());

  auto sym_result = this->DlSym(result.value(), TestSym("TestStart").c_str());
  ASSERT_TRUE(sym_result.is_ok()) << result.error_value();
  ASSERT_TRUE(sym_result.value());

  EXPECT_EQ(RunFunction<int64_t>(sym_result.value()), kReturnValue);

  ASSERT_TRUE(this->DlClose(result.value()).is_ok());
}

// Load a module that depends on a symbol provided directly by a dependency.
TYPED_TEST(DlTests, BasicDep) {
  constexpr int64_t kReturnValue = 17;
  const std::string kFile = TestModule("basic-dep");
  const std::string kDepFile = TestShlib("libld-dep-a");

  this->ExpectRootModule(kFile);
  this->Needed({kDepFile});

  auto result = this->DlOpen(kFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_ok()) << result.error_value();
  EXPECT_TRUE(result.value());

  auto sym_result = this->DlSym(result.value(), TestSym("TestStart").c_str());
  ASSERT_TRUE(sym_result.is_ok()) << result.error_value();
  ASSERT_TRUE(sym_result.value());

  EXPECT_EQ(RunFunction<int64_t>(sym_result.value()), kReturnValue);

  ASSERT_TRUE(this->DlClose(result.value()).is_ok());
}

// Load a module that depends on a symbols provided directly and transitively by
// several dependencies. Dependency ordering is serialized such that a module
// depends on a symbol provided by a dependency only one hop away
// (e.g. in its DT_NEEDED list):
TYPED_TEST(DlTests, IndirectDeps) {
  constexpr int64_t kReturnValue = 17;
  const std::string kFile = TestModule("indirect-deps");
  const std::string kDepFile1 = TestShlib("libindirect-deps-a");
  const std::string kDepFile2 = TestShlib("libindirect-deps-b");
  const std::string kDepFile3 = TestShlib("libindirect-deps-c");

  this->ExpectRootModule(kFile);
  this->Needed({kDepFile1, kDepFile2, kDepFile3});

  auto result = this->DlOpen(kFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_ok()) << result.error_value();
  EXPECT_TRUE(result.value());

  auto sym_result = this->DlSym(result.value(), TestSym("TestStart").c_str());
  ASSERT_TRUE(sym_result.is_ok()) << result.error_value();
  ASSERT_TRUE(sym_result.value());

  EXPECT_EQ(RunFunction<int64_t>(sym_result.value()), kReturnValue);

  ASSERT_TRUE(this->DlClose(result.value()).is_ok());
}

// Load a module that depends on symbols provided directly and transitively by
// several dependencies. Dependency ordering is DAG-like where several modules
// share a dependency.
TYPED_TEST(DlTests, ManyDeps) {
  constexpr int64_t kReturnValue = 17;
  const std::string kFile = TestModule("many-deps");
  const std::string kDepFile1 = TestShlib("libld-dep-a");
  const std::string kDepFile2 = TestShlib("libld-dep-b");
  const std::string kDepFile3 = TestShlib("libld-dep-f");
  const std::string kDepFile4 = TestShlib("libld-dep-c");
  const std::string kDepFile5 = TestShlib("libld-dep-d");
  const std::string kDepFile6 = TestShlib("libld-dep-e");

  this->ExpectRootModule(kFile);
  this->Needed({kDepFile1, kDepFile2, kDepFile3, kDepFile4, kDepFile5, kDepFile6});

  auto result = this->DlOpen(kFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_ok()) << result.error_value();
  EXPECT_TRUE(result.value());

  auto sym_result = this->DlSym(result.value(), TestSym("TestStart").c_str());
  ASSERT_TRUE(sym_result.is_ok()) << result.error_value();
  ASSERT_TRUE(sym_result.value());

  EXPECT_EQ(RunFunction<int64_t>(sym_result.value()), kReturnValue);

  ASSERT_TRUE(this->DlClose(result.value()).is_ok());
}

// TODO(https://fxbug.dev/339028040): Test missing symbol in transitive dep.
// Load a module that depends on libld-dep-a.so, but this dependency does not
// provide the c symbol referenced by the root module, so relocation fails.
TYPED_TEST(DlTests, MissingSymbol) {
  const std::string kFile = TestModule("missing-sym");
  const std::string kDepFile = TestShlib("libld-dep-missing-sym-dep");

  this->ExpectRootModule(kFile);
  this->Needed({kDepFile});

  auto result = this->DlOpen(kFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_error());
  EXPECT_THAT(result.error_value().take_str(),
              IsUndefinedSymbolErrMsg(TestSym("missing_sym"), kFile));
}

// TODO(https://fxbug.dev/3313662773): Test simple case of transitive missing
// symbol.
// dlopen missing-transitive-symbol:
//  - missing-transitive-sym
//    - has-missing-sym does not define missing_sym()
// call missing_sym() from missing-transitive-symbol, and expect symbol not found

// Try to load a module that has a (direct) dependency that cannot be found.
TYPED_TEST(DlTests, MissingDependency) {
  const std::string kFile = TestModule("missing-dep");
  const std::string kDepFile = TestShlib("libmissing-dep-dep");

  this->ExpectRootModule(kFile);
  this->Needed({NotFound(kDepFile)});

  auto result = this->DlOpen(kFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_error());

  // TODO(https://fxbug.dev/336633049): Harmonize "not found" error messages
  // between implementations.
  // Expect that the dependency lib to missing-dep.module.so cannot be found.
  if constexpr (TestFixture::kCanMatchExactError) {
    EXPECT_EQ(result.error_value().take_str(),
              "cannot open dependency: libmissing-dep-dep.MissingDependency.so");
  } else {
    EXPECT_THAT(
        result.error_value().take_str(),
        MatchesRegex(
            // emitted by Fuchsia-musl
            "Error loading shared library .*libmissing-dep-dep.MissingDependency.so: ZX_ERR_NOT_FOUND \\(needed by missing-dep.MissingDependency.module.so\\)"
            // emitted by Linux-glibc
            "|.*libmissing-dep-dep.MissingDependency.so: cannot open shared object file: No such file or directory"));
  }
}

// Try to load a module where the dependency of its direct dependency (i.e. a
// transitive dependency of the root module) cannot be found.
TYPED_TEST(DlTests, MissingTransitiveDependency) {
  const std::string kFile = TestModule("missing-transitive-dep");
  const std::string kDepFile1 = TestShlib("libhas-missing-dep");
  const std::string kDepFile2 = TestShlib("libmissing-dep-dep");

  this->ExpectRootModule(kFile);
  this->Needed({Found(kDepFile1), NotFound(kDepFile2)});

  auto result = this->DlOpen(kFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  // TODO(https://fxbug.dev/336633049): Harmonize "not found" error messages
  // between implementations.
  // Expect that the dependency lib to libhas-missing-dep.so cannot be found.
  if constexpr (TestFixture::kCanMatchExactError) {
    EXPECT_EQ(result.error_value().take_str(),
              "cannot open dependency: libmissing-dep-dep.MissingTransitiveDependency.so");
  } else {
    EXPECT_THAT(
        result.error_value().take_str(),
        MatchesRegex(
            // emitted by Fuchsia-musl
            "Error loading shared library .*libmissing-dep-dep.MissingTransitiveDependency.so: ZX_ERR_NOT_FOUND \\(needed by libhas-missing-dep.MissingTransitiveDependency.so\\)"
            // emitted by Linux-glibc
            "|.*libmissing-dep-dep.MissingTransitiveDependency.so: cannot open shared object file: No such file or directory"));
  }
}

// Test loading a module with relro protections.
TYPED_TEST(DlTests, Relro) {
  const std::string kFile = TestModule("relro");

  this->ExpectRootModule(kFile);

  auto result = this->DlOpen(kFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_ok()) << result.error_value();
  ASSERT_TRUE(result.value());

  auto sym = this->DlSym(result.value(), TestSym("TestStart").c_str());
  ASSERT_TRUE(sym.is_ok()) << sym.error_value();
  ASSERT_TRUE(sym.value());

  EXPECT_DEATH(RunFunction<int64_t>(sym.value()), "");

  ASSERT_TRUE(this->DlClose(result.value()).is_ok());
}

// Test that calling dlopen twice on a file will return the same pointer,
// indicating that the dynamic linker is storing the module in its bookkeeping.
// dlsym() should return a pointer to the same symbol from the same module as
// well.
TYPED_TEST(DlTests, BasicModuleReuse) {
  const std::string kFile = TestModule("ret17");

  this->ExpectRootModule(kFile);

  auto res1 = this->DlOpen(kFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res1.is_ok()) << res1.error_value();
  auto ptr1 = res1.value();
  EXPECT_TRUE(ptr1);

  auto res2 = this->DlOpen(kFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res2.is_ok()) << res2.error_value();
  auto ptr2 = res2.value();
  EXPECT_TRUE(ptr2);

  EXPECT_EQ(ptr1, ptr2);

  auto sym1 = this->DlSym(ptr1, TestSym("TestStart").c_str());
  ASSERT_TRUE(sym1.is_ok()) << sym1.error_value();
  auto sym1_ptr = sym1.value();
  EXPECT_TRUE(sym1_ptr);

  auto sym2 = this->DlSym(ptr2, TestSym("TestStart").c_str());
  ASSERT_TRUE(sym2.is_ok()) << sym2.error_value();
  auto sym2_ptr = sym2.value();
  EXPECT_TRUE(sym2_ptr);

  EXPECT_EQ(sym1_ptr, sym2_ptr);

  ASSERT_TRUE(this->DlClose(ptr1).is_ok());
  ASSERT_TRUE(this->DlClose(ptr2).is_ok());
}

// Test that different mutually-exclusive kFiles that were dlopen-ed do not share
// pointers or resolved symbols.
TYPED_TEST(DlTests, UniqueModules) {
  const std::string kFile1 = TestModule("ret17");
  const std::string kFile2 = TestModule("ret23");

  this->ExpectRootModule(kFile1);

  auto ret17 = this->DlOpen(kFile1.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(ret17.is_ok()) << ret17.error_value();
  auto ret17_ptr = ret17.value();
  EXPECT_TRUE(ret17_ptr);

  this->ExpectRootModule(kFile2);

  auto ret23 = this->DlOpen(kFile2.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(ret23.is_ok()) << ret23.error_value();
  auto ret23_ptr = ret23.value();
  EXPECT_TRUE(ret23_ptr);

  EXPECT_NE(ret17_ptr, ret23_ptr);

  auto sym17 = this->DlSym(ret17_ptr, TestSym("TestStart").c_str());
  ASSERT_TRUE(sym17.is_ok()) << sym17.error_value();
  auto sym17_ptr = sym17.value();
  EXPECT_TRUE(sym17_ptr);

  auto sym23 = this->DlSym(ret23_ptr, TestSym("TestStart").c_str());
  ASSERT_TRUE(sym23.is_ok()) << sym23.error_value();
  auto sym23_ptr = sym23.value();
  EXPECT_TRUE(sym23_ptr);

  EXPECT_NE(sym17_ptr, sym23_ptr);

  EXPECT_EQ(RunFunction<int64_t>(sym17_ptr), 17);
  EXPECT_EQ(RunFunction<int64_t>(sym23_ptr), 23);

  ASSERT_TRUE(this->DlClose(ret17_ptr).is_ok());
  ASSERT_TRUE(this->DlClose(ret23_ptr).is_ok());
}

// Test that you can dlopen a dependency from a previously loaded module.
TYPED_TEST(DlTests, OpenDepDirectly) {
  const std::string kFile = TestShlib("libhas-foo-v1");
  const std::string kDepFile = TestShlib("libld-dep-foo-v1");

  this->Needed({kFile, kDepFile});

  auto res1 = this->DlOpen(kFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res1.is_ok()) << res1.error_value();
  EXPECT_TRUE(res1.value());

  // dlopen kDepFile expecting it to already be loaded.
  auto res2 = this->DlOpen(kDepFile.c_str(), RTLD_NOW | RTLD_LOCAL | RTLD_NOLOAD);
  ASSERT_TRUE(res2.is_ok()) << res2.error_value();
  EXPECT_TRUE(res2.value());

  // Test that dlsym will resolve the same symbol pointer from the shared
  // dependency between kFile (res1) and kDepFile (res2).
  auto sym1 = this->DlSym(res1.value(), TestSym("foo").c_str());
  ASSERT_TRUE(sym1.is_ok()) << sym1.error_value();
  ASSERT_TRUE(sym1.value());

  auto sym2 = this->DlSym(res2.value(), TestSym("foo").c_str());
  ASSERT_TRUE(sym2.is_ok()) << sym2.error_value();
  ASSERT_TRUE(sym2.value());

  EXPECT_EQ(sym1.value(), sym2.value());

  EXPECT_EQ(RunFunction<int64_t>(sym1.value()), RunFunction<int64_t>(sym2.value()));

  ASSERT_TRUE(this->DlClose(res1.value()).is_ok());
  ASSERT_TRUE(this->DlClose(res2.value()).is_ok());
}

// These are test scenarios that test symbol resolution from just the dependency
// graph, ie from the local scope of the dlopen-ed module.

// Test that dep ordering is preserved in the dependency graph.
// dlopen multiple-foo-deps -> calls foo()
//  - foo-v1 -> foo() returns 2
//  - foo-v2 -> foo() returns 7
// call foo() from multiple-foo-deps pointer and expect 2 from foo-v1.
TYPED_TEST(DlTests, DepOrder) {
  constexpr const int64_t kReturnValue = 2;
  const std::string kFile = TestModule("multiple-foo-deps");
  const std::string kDepFile1 = TestShlib("libld-dep-foo-v1");
  const std::string kDepFile2 = TestShlib("libld-dep-foo-v2");

  this->ExpectRootModule(kFile);
  this->Needed({kDepFile1, kDepFile2});

  auto res = this->DlOpen(kFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res.is_ok()) << res.error_value();
  EXPECT_TRUE(res.value());

  auto sym = this->DlSym(res.value(), TestSym("call_foo").c_str());
  ASSERT_TRUE(sym.is_ok()) << sym.error_value();
  ASSERT_TRUE(sym.value());

  EXPECT_EQ(RunFunction<int64_t>(sym.value()), kReturnValue);

  ASSERT_TRUE(this->DlClose(res.value()).is_ok());
}

// Test that transitive dep ordering is preserved the dependency graph.
// dlopen transitive-foo-dep -> calls foo()
//   - has-foo-v1:
//     - foo-v1 -> foo() returns 2
//   - foo-v2 -> foo() returns 7
// call foo() from transitive-foo-dep pointer and expect 7 from foo-v2.
TYPED_TEST(DlTests, TransitiveDepOrder) {
  constexpr const int64_t kReturnValue = 7;
  const std::string kFile = TestModule("transitive-foo-dep");
  const std::string kDepFile1 = TestShlib("libhas-foo-v1");
  const std::string kDepFile2 = TestShlib("libld-dep-foo-v2");
  const std::string kDepFile3 = TestShlib("libld-dep-foo-v1");

  this->ExpectRootModule(kFile);
  this->Needed({kDepFile1, kDepFile2, kDepFile3});

  auto res = this->DlOpen(kFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res.is_ok()) << res.error_value();
  EXPECT_TRUE(res.value());

  auto sym = this->DlSym(res.value(), TestSym("call_foo").c_str());
  ASSERT_TRUE(sym.is_ok()) << sym.error_value();
  ASSERT_TRUE(sym.value());

  EXPECT_EQ(RunFunction<int64_t>(sym.value()), kReturnValue);

  ASSERT_TRUE(this->DlClose(res.value()).is_ok());
}

// Test that a module graph with a dependency cycle involving the root module
// will complete dlopen() & dlsym() calls.
// dlopen cyclic-dep-parent: foo() returns 2.
//   - has-cyclic-dep:
//     - cyclic-dep-parent
//     - foo-v1: foo() returns 2.
TYPED_TEST(DlTests, CyclicalDependency) {
  constexpr const int64_t kReturnValue = 2;
  const auto kFile = TestModule("cyclic-dep-parent");
  const auto kDepFile1 = TestShlib("libhas-cyclic-dep");
  const auto kDepFile2 = TestShlib("libld-dep-foo-v1");

  this->ExpectRootModule(kFile);
  this->Needed({kDepFile1, kDepFile2});

  auto res = this->DlOpen(kFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res.is_ok()) << res.error_value();
  EXPECT_TRUE(res.value());

  auto sym = this->DlSym(res.value(), TestSym("call_foo").c_str());
  ASSERT_TRUE(sym.is_ok()) << sym.error_value();
  ASSERT_TRUE(sym.value());

  EXPECT_EQ(RunFunction<int64_t>(sym.value()), kReturnValue);

  ASSERT_TRUE(this->DlClose(res.value()).is_ok());
}

// These are test scenarios that test symbol resolution from the local scope
// of the dlopen-ed module, when some (or all) of its dependencies have already
// been loaded.

// Test that dependency ordering is always preserved in the local symbol scope,
// even if a module with the same symbol was already loaded (with RTLD_LOCAL).
// dlopen foo-v2 -> foo() returns 7
// dlopen multiple-foo-deps:
//   - foo-v1 -> foo() returns 2
//   - foo-v2 -> foo() returns 7
// call foo() from multiple-foo-deps and expect 2 from foo-v1 because it is
// first in multiple-foo-deps local scope.
TYPED_TEST(DlTests, LocalPrecedence) {
  const std::string kFile = TestModule("multiple-foo-deps");
  const std::string kDepFile1 = TestShlib("libld-dep-foo-v1");
  const std::string kDepFile2 = TestShlib("libld-dep-foo-v2");
  constexpr int64_t kReturnValueFromFooV1 = 2;
  constexpr int64_t kReturnValueFromFooV2 = 7;

  this->Needed({kDepFile2});

  auto res1 = this->DlOpen(kDepFile2.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res1.is_ok()) << res1.error_value();
  EXPECT_TRUE(res1.value());

  auto sym1 = this->DlSym(res1.value(), TestSym("foo").c_str());
  ASSERT_TRUE(sym1.is_ok()) << sym1.error_value();
  ASSERT_TRUE(sym1.value());

  EXPECT_EQ(RunFunction<int64_t>(sym1.value()), kReturnValueFromFooV2);

  this->ExpectRootModule(kFile);

  this->Needed({kDepFile1});

  auto res2 = this->DlOpen(kFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res2.is_ok()) << res2.error_value();
  EXPECT_TRUE(res2.value());

  // Test the `foo` value that is used by the root module's `call_foo()` function.
  auto sym2 = this->DlSym(res2.value(), TestSym("call_foo").c_str());
  ASSERT_TRUE(sym2.is_ok()) << sym2.error_value();
  ASSERT_TRUE(sym2.value());

  if constexpr (TestFixture::kStrictLoadOrderPriority) {
    // Musl will prioritize the symbol that was loaded first (from libfoo-v2),
    // even though the file does not have global scope.
    EXPECT_EQ(RunFunction<int64_t>(sym2.value()), kReturnValueFromFooV2);
  } else {
    // Glibc & libdl will use the symbol that is first encountered in the
    // module's local scope, from libfoo-v1.
    EXPECT_EQ(RunFunction<int64_t>(sym2.value()), kReturnValueFromFooV1);
  }

  // dlsym will always use dependency ordering from the local scope when looking
  // up a symbol. Unlike `call_foo()`, `foo()` is provided by a dependency, and
  // both musl and glibc will only look for this symbol in the root module's
  // local-scope dependency set.
  auto sym3 = this->DlSym(res2.value(), TestSym("foo").c_str());
  ASSERT_TRUE(sym3.is_ok()) << sym3.error_value();
  ASSERT_TRUE(sym3.value());

  EXPECT_EQ(RunFunction<int64_t>(sym3.value()), kReturnValueFromFooV1);

  ASSERT_TRUE(this->DlClose(res1.value()).is_ok());
  ASSERT_TRUE(this->DlClose(res2.value()).is_ok());
}

// Test the local symbol scope precedence is not affected by transitive
// dependencies that were already loaded.
// dlopen has-foo-v1:
//  - foo-v1 -> foo() returns 2
// dlopen transitive-foo-dep:
//   - has-foo-v1:
//     - foo-v1 -> foo() returns 2
//   - foo-v2 -> foo() returns 7
// call foo() from multiple-transitive-foo-deps and expect 7 from foo-v2 because
// it is first in multiple-transitive-foo-dep's local scope.
TYPED_TEST(DlTests, LocalPrecedenceTransitiveDeps) {
  const std::string kFile = TestModule("transitive-foo-dep");
  const std::string kDepFile1 = TestShlib("libhas-foo-v1");
  const std::string kDepFile2 = TestShlib("libld-dep-foo-v1");
  const std::string kDepFile3 = TestShlib("libld-dep-foo-v2");
  constexpr int64_t kReturnValueFromFooV1 = 2;
  constexpr int64_t kReturnValueFromFooV2 = 7;

  this->Needed({kDepFile1, kDepFile2});

  auto res1 = this->DlOpen(kDepFile1.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res1.is_ok()) << res1.error_value();
  EXPECT_TRUE(res1.value());

  auto sym1 = this->DlSym(res1.value(), TestSym("call_foo").c_str());
  ASSERT_TRUE(sym1.is_ok()) << sym1.error_value();
  ASSERT_TRUE(sym1.value());

  EXPECT_EQ(RunFunction<int64_t>(sym1.value()), kReturnValueFromFooV1);

  this->ExpectRootModule(kFile);
  this->Needed({kDepFile3});

  auto res2 = this->DlOpen(kFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res2.is_ok()) << res2.error_value();
  EXPECT_TRUE(res2.value());

  auto sym2 = this->DlSym(res2.value(), TestSym("call_foo").c_str());
  ASSERT_TRUE(sym2.is_ok()) << sym2.error_value();
  ASSERT_TRUE(sym2.value());

  if (TestFixture::kStrictLoadOrderPriority) {
    // Musl will prioritize the symbol that was loaded first (from has-foo-v1 +
    // foo-v1), even though the file does not have global scope.
    EXPECT_EQ(RunFunction<int64_t>(sym2.value()), kReturnValueFromFooV1);
  } else {
    // Glibc & libdl will use the symbol that is first encountered in the
    // module's local scope, from foo-v2.
    EXPECT_EQ(RunFunction<int64_t>(sym2.value()), kReturnValueFromFooV2);
  }

  ASSERT_TRUE(this->DlClose(res1.value()).is_ok());
  ASSERT_TRUE(this->DlClose(res2.value()).is_ok());
}

// The following tests will test dlopen-ing a module and resolving symbols from
// transitive dependency modules, all of which have already been loaded.
// dlopen has-foo-v2:
//  - foo-v2 -> foo() returns 7
// dlopen has-foo-v1:
//  - foo-v1 -> foo() returns 2
// dlopen multiple-transitive-foo-deps:
//   - has-foo-v1:
//     - foo-v1 -> foo() returns 2
//   - has-foo-v2:
//     - foo-v2 -> foo() returns 7
// Call foo() from multiple-transitive-foo-deps and expect 2 from foo-v1,
// because it is first in multiple-transitive-foo-dep's local scope.
TYPED_TEST(DlTests, LoadedTransitiveDepOrder) {
  const std::string kFile = TestModule("multiple-transitive-foo-deps");
  const std::string kDepFile1 = TestShlib("libhas-foo-v2");
  const std::string kDepFile2 = TestShlib("libld-dep-foo-v2");
  const std::string kDepFile3 = TestShlib("libhas-foo-v1");
  const std::string kDepFile4 = TestShlib("libld-dep-foo-v1");
  constexpr int64_t kReturnValueFromFooV1 = 2;
  constexpr int64_t kReturnValueFromFooV2 = 7;

  this->Needed({kDepFile1, kDepFile2});

  auto res1 = this->DlOpen(kDepFile1.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res1.is_ok()) << res1.error_value();
  EXPECT_TRUE(res1.value());

  auto sym1 = this->DlSym(res1.value(), TestSym("call_foo").c_str());
  ASSERT_TRUE(sym1.is_ok()) << sym1.error_value();
  ASSERT_TRUE(sym1.value());

  EXPECT_EQ(RunFunction<int64_t>(sym1.value()), kReturnValueFromFooV2);

  this->Needed({kDepFile3, kDepFile4});

  auto res2 = this->DlOpen(kDepFile3.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res2.is_ok()) << res2.error_value();
  EXPECT_TRUE(res2.value());

  auto sym2 = this->DlSym(res2.value(), TestSym("call_foo").c_str());
  ASSERT_TRUE(sym2.is_ok()) << sym2.error_value();
  ASSERT_TRUE(sym2.value());

  EXPECT_EQ(RunFunction<int64_t>(sym2.value()), kReturnValueFromFooV1);

  this->ExpectRootModule(kFile);

  auto res3 = this->DlOpen(kFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res3.is_ok()) << res3.error_value();
  EXPECT_TRUE(res3.value());

  auto sym3 = this->DlSym(res3.value(), TestSym("call_foo").c_str());
  ASSERT_TRUE(sym3.is_ok()) << sym3.error_value();
  ASSERT_TRUE(sym3.value());

  // Both Glibc & Musl's agree on the resolved symbol here here, because
  // has-foo-v1 is looked up first (and it is already loaded), so its symbols
  // take precedence.
  EXPECT_EQ(RunFunction<int64_t>(sym3.value()), kReturnValueFromFooV1);

  ASSERT_TRUE(this->DlClose(res1.value()).is_ok());
  ASSERT_TRUE(this->DlClose(res2.value()).is_ok());
  ASSERT_TRUE(this->DlClose(res3.value()).is_ok());
}

// Whereas the above tests test how symbols are resolved for the root module,
// the following tests will test how symbols are resolved for dependencies of
// the root module.

// Test that the load-order of the local scope has precedence in the symbol
// resolution for symbols used by dependencies (i.e. a sibling's symbol is used
// used when it's earlier in the load-order set).
// dlopen precedence-in-dep-resolution:
//   - bar-v1 -> bar_v1() calls foo().
//      - foo-v1 -> foo() returns 2
//   - bar-v2 -> bar_v2() calls foo().
//      - foo-v2 -> foo() returns 7
// bar_v1() from precedence-in-dep-resolution and expect 2.
// bar_v2() from precedence-in-dep-resolution and expect 2 from foo-v1.
TYPED_TEST(DlTests, PrecedenceInDepResolution) {
  const std::string kFile = TestModule("precedence-in-dep-resolution");
  const std::string kDepFile1 = TestShlib("libbar-v1");
  const std::string kDepFile2 = TestShlib("libbar-v2");
  const std::string kDepFile3 = TestShlib("libld-dep-foo-v1");
  const std::string kDepFile4 = TestShlib("libld-dep-foo-v2");
  constexpr int64_t kReturnValueFromFooV1 = 2;

  this->ExpectRootModule(kFile);
  this->Needed({kDepFile1, kDepFile2, kDepFile3, kDepFile4});

  auto res1 = this->DlOpen(kFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res1.is_ok()) << res1.error_value();
  EXPECT_TRUE(res1.value());

  auto bar_v1 = this->DlSym(res1.value(), TestSym("bar_v1").c_str());
  ASSERT_TRUE(bar_v1.is_ok()) << bar_v1.error_value();
  ASSERT_TRUE(bar_v1.value());

  EXPECT_EQ(RunFunction<int64_t>(bar_v1.value()), kReturnValueFromFooV1);

  auto bar_v2 = this->DlSym(res1.value(), TestSym("bar_v2").c_str());
  ASSERT_TRUE(bar_v2.is_ok()) << bar_v2.error_value();
  ASSERT_TRUE(bar_v2.value());

  EXPECT_EQ(RunFunction<int64_t>(bar_v2.value()), kReturnValueFromFooV1);

  ASSERT_TRUE(this->DlClose(res1.value()).is_ok());
}

// Test that the root module symbols have precedence in the symbol resolution
// for symbols used by dependencies.
// dlopen root-precedence-in-dep-resolution: foo() returns 17
//   - bar-v1 -> bar_v1() calls foo().
//      - foo-v1 -> foo() returns 2
//   - bar-v2 -> bar_v2() calls foo().
//      - foo-v2 -> foo() returns 7
// call foo() from root-precedence-in-dep-resolution and expect 17.
// bar_v1() from root-precedence-in-dep-resolution and expect 17.
// bar_v2() from root-precedence-in-dep-resolution and expect 17.
TYPED_TEST(DlTests, RootPrecedenceInDepResolution) {
  const std::string kFile = TestModule("root-precedence-in-dep-resolution");
  const std::string kDepFile1 = TestShlib("libbar-v1");
  const std::string kDepFile2 = TestShlib("libbar-v2");
  const std::string kDepFile3 = TestShlib("libld-dep-foo-v1");
  const std::string kDepFile4 = TestShlib("libld-dep-foo-v2");
  constexpr int64_t kReturnValueFromRootModule = 17;
  constexpr int64_t kReturnValueFromFooV1 = 2;
  constexpr int64_t kReturnValueFromFooV2 = 7;

  this->ExpectRootModule(kFile);
  this->Needed({kDepFile1, kDepFile2, kDepFile3, kDepFile4});

  auto res1 = this->DlOpen(kFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res1.is_ok()) << res1.error_value();
  EXPECT_TRUE(res1.value());

  auto foo = this->DlSym(res1.value(), TestSym("foo").c_str());
  ASSERT_TRUE(foo.is_ok()) << foo.error_value();
  ASSERT_TRUE(foo.value());

  EXPECT_EQ(RunFunction<int64_t>(foo.value()), kReturnValueFromRootModule);

  auto bar_v1 = this->DlSym(res1.value(), TestSym("bar_v1").c_str());
  ASSERT_TRUE(bar_v1.is_ok()) << bar_v1.error_value();
  ASSERT_TRUE(bar_v1.value());

  EXPECT_EQ(RunFunction<int64_t>(bar_v1.value()), kReturnValueFromRootModule);

  auto bar_v2 = this->DlSym(res1.value(), TestSym("bar_v2").c_str());
  ASSERT_TRUE(bar_v2.is_ok()) << bar_v2.error_value();
  ASSERT_TRUE(bar_v2.value());

  EXPECT_EQ(RunFunction<int64_t>(bar_v2.value()), kReturnValueFromRootModule);

  // Test that when we dlopen the dep directly, foo is resolved to the
  // transitive dependency, while bar_v1/bar_v2 continue to use the root
  // module's foo symbol.
  auto dep1 = this->DlOpen(kDepFile1.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(dep1.is_ok()) << dep1.error_value();
  EXPECT_TRUE(dep1.value());

  auto foo1 = this->DlSym(dep1.value(), TestSym("foo").c_str());
  ASSERT_TRUE(foo1.is_ok()) << foo1.error_value();
  ASSERT_TRUE(foo1.value());

  EXPECT_EQ(RunFunction<int64_t>(foo1.value()), kReturnValueFromFooV1);

  auto dep_bar1 = this->DlSym(dep1.value(), TestSym("bar_v1").c_str());
  ASSERT_TRUE(dep_bar1.is_ok()) << dep_bar1.error_value();
  ASSERT_TRUE(dep_bar1.value());

  EXPECT_EQ(RunFunction<int64_t>(dep_bar1.value()), kReturnValueFromRootModule);

  auto dep2 = this->DlOpen(kDepFile2.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(dep2.is_ok()) << dep2.error_value();
  EXPECT_TRUE(dep2.value());

  auto foo2 = this->DlSym(dep2.value(), TestSym("foo").c_str());
  ASSERT_TRUE(foo2.is_ok()) << foo2.error_value();
  ASSERT_TRUE(foo2.value());

  EXPECT_EQ(RunFunction<int64_t>(foo2.value()), kReturnValueFromFooV2);

  auto dep_bar2 = this->DlSym(dep2.value(), TestSym("bar_v2").c_str());
  ASSERT_TRUE(dep_bar2.is_ok()) << dep_bar2.error_value();
  ASSERT_TRUE(dep_bar2.value());

  EXPECT_EQ(RunFunction<int64_t>(dep_bar2.value()), kReturnValueFromRootModule);

  ASSERT_TRUE(this->DlClose(dep1.value()).is_ok());
  ASSERT_TRUE(this->DlClose(dep2.value()).is_ok());
  ASSERT_TRUE(this->DlClose(res1.value()).is_ok());
}

// These are test scenarios that test symbol resolution with RTLD_GLOBAL.

// Test that a non-global module can depend on a previously loaded global
// module, and that dlsym() will be able to access the global module's symbols
// from the non-global module.
// dlopen RTLD_GLOBAL foo-v2 -> foo() returns 7
// dlopen has-foo-v2:
//    - foo-v2 -> foo() returns 7
// call foo() from has-foo-v2 and expect foo() to return 7 (from previously
// loaded RTLD_GLOBAL foo-v2).
TYPED_TEST(DlTests, GlobalDep) {
  const auto kGlobalDepFile = TestShlib("libld-dep-foo-v2");
  const auto kParentFile = TestShlib("libhas-foo-v2");
  constexpr int64_t kReturnValueFromGlobalDep = 7;

  this->Needed({kGlobalDepFile});

  auto open_global_dep = this->DlOpen(kGlobalDepFile.c_str(), RTLD_NOW | RTLD_GLOBAL);
  ASSERT_TRUE(open_global_dep.is_ok()) << open_global_dep.error_value();
  EXPECT_TRUE(open_global_dep.value());

  auto global_foo = this->DlSym(open_global_dep.value(), TestSym("foo").c_str());
  ASSERT_TRUE(global_foo.is_ok()) << global_foo.error_value();
  ASSERT_TRUE(global_foo.value());

  EXPECT_EQ(RunFunction<int64_t>(global_foo.value()), kReturnValueFromGlobalDep);

  this->Needed({kParentFile});

  auto open_parent = this->DlOpen(kParentFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(open_parent.is_ok()) << open_parent.error_value();
  EXPECT_TRUE(open_parent.value());

  auto parent_call_foo = this->DlSym(open_parent.value(), TestSym("call_foo").c_str());
  ASSERT_TRUE(parent_call_foo.is_ok()) << parent_call_foo.error_value();
  ASSERT_TRUE(parent_call_foo.value());

  EXPECT_EQ(RunFunction<int64_t>(parent_call_foo.value()), kReturnValueFromGlobalDep);

  auto parent_foo = this->DlSym(open_parent.value(), TestSym("foo").c_str());
  ASSERT_TRUE(parent_foo.is_ok()) << parent_foo.error_value();
  ASSERT_TRUE(parent_foo.value());

  EXPECT_EQ(RunFunction<int64_t>(parent_foo.value()), kReturnValueFromGlobalDep);

  ASSERT_TRUE(this->DlClose(open_global_dep.value()).is_ok());
  ASSERT_TRUE(this->DlClose(open_parent.value()).is_ok());
}

// Test that loaded global module will take precedence over dependency ordering.
// dlopen RTLD_GLOBAL foo-v2 -> foo() returns 7
// dlopen has-foo-v1:
//    - foo-v1 -> foo() returns 2
// call foo() from has-foo-v1 and expect foo() to return 7 (from previously
// loaded RTLD_GLOBAL foo-v2).
TYPED_TEST(DlTests, GlobalPrecedence) {
  const std::string kFile1 = TestShlib("libld-dep-foo-v2");
  const std::string kFile2 = TestShlib("libhas-foo-v1");
  const std::string kDepFile = TestShlib("libld-dep-foo-v1");
  constexpr int64_t kReturnValueFromFooV1 = 2;
  constexpr int64_t kReturnValueFromFooV2 = 7;

  this->Needed({kFile1});

  auto res1 = this->DlOpen(kFile1.c_str(), RTLD_NOW | RTLD_GLOBAL);
  ASSERT_TRUE(res1.is_ok()) << res1.error_value();
  EXPECT_TRUE(res1.value());

  auto sym1 = this->DlSym(res1.value(), TestSym("foo").c_str());
  ASSERT_TRUE(sym1.is_ok()) << sym1.error_value();
  ASSERT_TRUE(sym1.value());

  EXPECT_EQ(RunFunction<int64_t>(sym1.value()), kReturnValueFromFooV2);

  this->Needed({kFile2, kDepFile});

  auto res2 = this->DlOpen(kFile2.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res2.is_ok()) << res2.error_value();
  EXPECT_TRUE(res2.value());

  auto sym2 = this->DlSym(res2.value(), TestSym("call_foo").c_str());
  ASSERT_TRUE(sym2.is_ok()) << sym2.error_value();
  ASSERT_TRUE(sym2.value());

  EXPECT_EQ(RunFunction<int64_t>(sym2.value()), kReturnValueFromFooV2);

  // dlsym will always use dependency ordering from the local scope when looking
  // up a symbol
  auto sym3 = this->DlSym(res2.value(), TestSym("foo").c_str());
  ASSERT_TRUE(sym3.is_ok()) << sym3.error_value();
  ASSERT_TRUE(sym3.value());

  EXPECT_EQ(RunFunction<int64_t>(sym3.value()), kReturnValueFromFooV1);

  ASSERT_TRUE(this->DlClose(res1.value()).is_ok());
  ASSERT_TRUE(this->DlClose(res2.value()).is_ok());
}

// Test that RTLD_GLOBAL applies to deps and load order will take precedence in
// subsequent symbol lookups:
// dlopen RTLD_GLOBAL has-foo-v1:
//   - foo-v1 -> foo() returns 2
// dlopen RTLD_GLOBAL has-foo-v2:
//   - foo-v2 -> foo() returns 7
// call foo from has-foo-v2 and expect 2.
TYPED_TEST(DlTests, GlobalPrecedenceDeps) {
  const std::string kFile1 = TestShlib("libhas-foo-v1");
  const std::string kDepFile1 = TestShlib("libld-dep-foo-v1");
  const std::string kFile2 = TestShlib("libhas-foo-v2");
  const std::string kDepFile2 = TestShlib("libld-dep-foo-v2");
  constexpr int64_t kReturnValueFromFooV1 = 2;
  constexpr int64_t kReturnValueFromFooV2 = 7;

  this->Needed({kFile1, kDepFile1});

  auto res1 = this->DlOpen(kFile1.c_str(), RTLD_NOW | RTLD_GLOBAL);
  ASSERT_TRUE(res1.is_ok()) << res1.error_value();
  EXPECT_TRUE(res1.value());

  auto sym1 = this->DlSym(res1.value(), TestSym("call_foo").c_str());
  ASSERT_TRUE(sym1.is_ok()) << sym1.error_value();
  ASSERT_TRUE(sym1.value());

  EXPECT_EQ(RunFunction<int64_t>(sym1.value()), kReturnValueFromFooV1);

  this->Needed({kFile2, kDepFile2});

  auto res2 = this->DlOpen(kFile2.c_str(), RTLD_NOW | RTLD_GLOBAL);
  ASSERT_TRUE(res2.is_ok()) << res2.error_value();
  EXPECT_TRUE(res2.value());

  auto sym2 = this->DlSym(res2.value(), TestSym("call_foo").c_str());
  ASSERT_TRUE(sym2.is_ok()) << sym2.error_value();
  ASSERT_TRUE(sym2.value());

  EXPECT_EQ(RunFunction<int64_t>(sym2.value()), kReturnValueFromFooV1);

  // dlsym will always use dependency ordering from the local scope when looking
  // up a symbol
  auto sym3 = this->DlSym(res2.value(), TestSym("foo").c_str());
  ASSERT_TRUE(sym3.is_ok()) << sym3.error_value();
  ASSERT_TRUE(sym3.value());

  EXPECT_EQ(RunFunction<int64_t>(sym3.value()), kReturnValueFromFooV2);

  ASSERT_TRUE(this->DlClose(res1.value()).is_ok());
  ASSERT_TRUE(this->DlClose(res2.value()).is_ok());
}

// Test that missing dep will use global symbol if there's a loaded global
// module with the same symbol
// dlopen RTLD global defines-missing-sym -> missing_sym() returns 2
// dlopen missing-sym -> TestStart() returns 2 + missing_sym():
//  - missing-sym-dep (does not have missing_sym())
// call missing_sym() from missing-sym and expect 6 (4 + 2 from previously
// loaded module).
TYPED_TEST(DlTests, GlobalSatisfiesMissingSymbol) {
  const std::string kFile1 = TestShlib("libld-dep-defines-missing-sym");
  const std::string kFile2 = TestModule("missing-sym");
  const std::string kDepFile = TestShlib("libld-dep-missing-sym-dep");
  constexpr int64_t kReturnValue = 6;

  this->Needed({kFile1});

  auto res1 = this->DlOpen(kFile1.c_str(), RTLD_NOW | RTLD_GLOBAL);
  ASSERT_TRUE(res1.is_ok()) << res1.error_value();
  EXPECT_TRUE(res1.value());

  this->ExpectRootModule(kFile2);
  this->Needed({kDepFile});

  auto res2 = this->DlOpen(kFile2.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res2.is_ok()) << res2.error_value();
  EXPECT_TRUE(res2.value());

  auto sym2 = this->DlSym(res2.value(), TestSym("TestStart").c_str());
  ASSERT_TRUE(sym2.is_ok()) << sym2.error_value();
  ASSERT_TRUE(sym2.value());

  EXPECT_EQ(RunFunction<int64_t>(sym2.value()), kReturnValue);

  const std::string symbol_name = TestSym("missing-sym");

  // dlsym will not be able to find the global symbol from the local scope
  auto sym3 = this->DlSym(res2.value(), symbol_name.c_str());
  ASSERT_TRUE(sym3.is_error()) << sym3.error_value();
  EXPECT_THAT(sym3.error_value().take_str(), IsUndefinedSymbolErrMsg(symbol_name, kFile2));

  ASSERT_TRUE(this->DlClose(res1.value()).is_ok());
  ASSERT_TRUE(this->DlClose(res2.value()).is_ok());
}

// Test dlopen-ing a previously non-global loaded module with RTLD_GLOBAL will
// make the module and all of its deps global.
// dlopen has-foo-v1:
//   - foo-v1 -> foo() returns 2
// dlopen has-foo-v2:
//   - foo-v2 -> foo() returns 7
// dlopen RTLD_GLOBAL has-foo-v2.
// dlopen bar-v1 -> bar_v1() calls foo().
//      - foo-v1 -> foo() returns 2
// Call foo() from bar_v1() and get 7 from RTLD_GLOBAL foo-v2
TYPED_TEST(DlTests, UpdateModeToGlobal) {
  const std::string kHasFooV1File = TestShlib("libhas-foo-v1");
  const std::string kFooV1DepFile = TestShlib("libld-dep-foo-v1");
  const std::string kHasFooV2File = TestShlib("libhas-foo-v2");
  const std::string kFooV2DepFile = TestShlib("libld-dep-foo-v2");
  const std::string kBarV1File = TestShlib("libbar-v1");
  constexpr int64_t kReturnValueFromFooV1 = 2;
  constexpr int64_t kReturnValueFromFooV2 = 7;

  this->Needed({kHasFooV1File, kFooV1DepFile});

  auto local_foo_v1_open = this->DlOpen(kHasFooV1File.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(local_foo_v1_open.is_ok()) << local_foo_v1_open.error_value();
  EXPECT_TRUE(local_foo_v1_open.value());

  auto local_foo_v1 = this->DlSym(local_foo_v1_open.value(), TestSym("foo").c_str());
  ASSERT_TRUE(local_foo_v1.is_ok()) << local_foo_v1.error_value();
  EXPECT_TRUE(local_foo_v1.value());

  // Confirm the resolved symbol is from the local dependency.
  EXPECT_EQ(RunFunction<int64_t>(local_foo_v1.value()), kReturnValueFromFooV1);

  this->Needed({kHasFooV2File, kFooV2DepFile});

  auto local_foo_v2_open = this->DlOpen(kHasFooV2File.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(local_foo_v2_open.is_ok()) << local_foo_v2_open.error_value();
  EXPECT_TRUE(local_foo_v2_open.value());

  auto local_foo_v2 = this->DlSym(local_foo_v2_open.value(), TestSym("foo").c_str());
  ASSERT_TRUE(local_foo_v2.is_ok()) << local_foo_v2.error_value();
  EXPECT_TRUE(local_foo_v2.value());

  // Confirm the resolved symbol is from the local dependency.
  EXPECT_EQ(RunFunction<int64_t>(local_foo_v2.value()), kReturnValueFromFooV2);

  // Dlopen the file and promote it and its dep (foo-v2) to global modules
  // (expecting the files to already be loaded).
  auto global_foo_v2_open =
      this->DlOpen(kHasFooV2File.c_str(), RTLD_NOW | RTLD_GLOBAL | RTLD_NOLOAD);
  ASSERT_TRUE(global_foo_v2_open.is_ok()) << global_foo_v2_open.error_value();
  EXPECT_TRUE(global_foo_v2_open.value());

  EXPECT_EQ(local_foo_v2_open.value(), global_foo_v2_open.value());

  this->Needed({kBarV1File});

  // Dlopen a new file that depends on a previously-loaded non-global dependency.
  auto local_bar_v1_open = this->DlOpen(kBarV1File.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(local_bar_v1_open.is_ok()) << local_bar_v1_open.error_value();
  EXPECT_TRUE(local_bar_v1_open.value());

  // Expect the resolved symbol to be provided by the global module.
  auto local_bar_v1 = this->DlSym(local_bar_v1_open.value(), TestSym("bar_v1").c_str());
  ASSERT_TRUE(local_bar_v1.is_ok()) << local_bar_v1.error_value();
  EXPECT_TRUE(local_bar_v1.value());

  if constexpr (TestFixture::kStrictLoadOrderPriority) {
    // Musl will prioritize the symbol that was loaded first (from foo-v1),
    // even though the file does not have global scope.
    EXPECT_EQ(RunFunction<int64_t>(local_bar_v1.value()), kReturnValueFromFooV1);
  } else {
    // Glibc & libdl will use the first global module that contains the symbol
    // (foo-v2)
    EXPECT_EQ(RunFunction<int64_t>(local_bar_v1.value()), kReturnValueFromFooV2);
  }

  ASSERT_TRUE(this->DlClose(local_foo_v1_open.value()).is_ok());
  ASSERT_TRUE(this->DlClose(local_foo_v2_open.value()).is_ok());
  ASSERT_TRUE(this->DlClose(local_bar_v1_open.value()).is_ok());
  ASSERT_TRUE(this->DlClose(global_foo_v2_open.value()).is_ok());
}

// Test that promoting a module to a global module, will change the "global load
// order" of the dynamic linker's bookkeeping list.
// dlopen foo-v1 -> foo() returns 2
// dlopen RTLD_GLOBAL foo-v2 -> foo() returns 7
// dlopen RTLD_GLOBAL foo-v1
// dlopen has-foo-v1
//  - foo-v1 -> foo() returns 7
// call foo() from has-foo-v1 and expect 2 from the first loaded global foo-v2
TYPED_TEST(DlTests, GlobalModuleOrdering) {
  const std::string kFooV1DepFile = TestShlib("libld-dep-foo-v1");
  const std::string kFooV2DepFile = TestShlib("libld-dep-foo-v2");
  const std::string kHasFooV1File = TestShlib("libhas-foo-v1");
  constexpr int64_t kReturnValueFromFooV1 = 2;
  constexpr int64_t kReturnValueFromFooV2 = 7;

  this->Needed({kFooV1DepFile});

  auto local_foo_v1_open = this->DlOpen(kFooV1DepFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(local_foo_v1_open.is_ok()) << local_foo_v1_open.error_value();
  EXPECT_TRUE(local_foo_v1_open.value());

  auto local_foo_v1 = this->DlSym(local_foo_v1_open.value(), TestSym("foo").c_str());
  ASSERT_TRUE(local_foo_v1.is_ok()) << local_foo_v1.error_value();
  EXPECT_TRUE(local_foo_v1.value());

  // Validity check foo() return value from foo-v1.
  EXPECT_EQ(RunFunction<int64_t>(local_foo_v1.value()), kReturnValueFromFooV1);

  this->Needed({kFooV2DepFile});

  auto global_foo_v2_open = this->DlOpen(kFooV2DepFile.c_str(), RTLD_NOW | RTLD_GLOBAL);
  ASSERT_TRUE(global_foo_v2_open.is_ok()) << global_foo_v2_open.error_value();
  EXPECT_TRUE(global_foo_v2_open.value());

  auto global_foo_v2 = this->DlSym(global_foo_v2_open.value(), TestSym("foo").c_str());
  ASSERT_TRUE(global_foo_v2.is_ok()) << global_foo_v2.error_value();
  EXPECT_TRUE(global_foo_v2.value());

  // Validity check foo() return value from foo-v2.
  EXPECT_EQ(RunFunction<int64_t>(global_foo_v2.value()), kReturnValueFromFooV2);

  // Promote foo-v1 to a global module (expecting it to already be loaded).
  auto global_foo_v1_open =
      this->DlOpen(kFooV1DepFile.c_str(), RTLD_NOW | RTLD_NOLOAD | RTLD_GLOBAL);
  ASSERT_TRUE(global_foo_v1_open.is_ok()) << global_foo_v1_open.error_value();
  EXPECT_TRUE(global_foo_v1_open.value());

  EXPECT_EQ(local_foo_v1_open.value(), global_foo_v1_open.value());

  this->Needed({kHasFooV1File});

  // Test that has-foo-v1 will now resolve its foo symbol from the recently
  // promoted global foo-v1 module because it comes first in the load order of
  // global modules.
  auto local_has_foo_v1_open = this->DlOpen(kHasFooV1File.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(local_has_foo_v1_open.is_ok()) << local_has_foo_v1_open.error_value();
  EXPECT_TRUE(local_has_foo_v1_open.value());

  auto local_has_foo_v1 = this->DlSym(local_has_foo_v1_open.value(), TestSym("call_foo").c_str());
  ASSERT_TRUE(local_has_foo_v1.is_ok()) << local_has_foo_v1.error_value();
  EXPECT_TRUE(local_has_foo_v1.value());

  if constexpr (TestFixture::kStrictLoadOrderPriority) {
    // Musl will prioritize the symbol from the (global) module that was loaded
    // first (foo-v1).
    EXPECT_EQ(RunFunction<int64_t>(local_has_foo_v1.value()), kReturnValueFromFooV1);
  } else {
    // Glibc will use the module that was loaded first _with_ RTLD_GLOBAL.
    EXPECT_EQ(RunFunction<int64_t>(local_has_foo_v1.value()), kReturnValueFromFooV2);
  }

  ASSERT_TRUE(this->DlClose(local_foo_v1_open.value()).is_ok());
  ASSERT_TRUE(this->DlClose(global_foo_v2_open.value()).is_ok());
  ASSERT_TRUE(this->DlClose(global_foo_v1_open.value()).is_ok());
  ASSERT_TRUE(this->DlClose(local_has_foo_v1_open.value()).is_ok());
}

// This tests that calling dlopen(..., RTLD_GLOBAL) multiple times on a module
// will not change the "global load order"
// dlopen RTLD_GLOBAL foo-v2 -> foo() returns 7
// dlopen RTLD_GLOBAL foo-v1 -> foo() returns 2
// dlopen RTLD_GLOBAL foo-v2 -> foo() returns 2
// dlopen bar-v1 -> bar_v1() calls foo().
//    - foo-v1 -> foo() returns 2
// Call foo() from bar-v1 and get 7 from global foo-v2, because foo-v2 was
// loaded first with RTLD_GLOBAL, and its order did not change.
TYPED_TEST(DlTests, GlobalModuleOrderingMultiDlopen) {
  const std::string kFooV2DepFile = TestShlib("libld-dep-foo-v2");
  const std::string kFooV1DepFile = TestShlib("libld-dep-foo-v1");
  const std::string kBarV2File = TestShlib("libbar-v1");
  constexpr int64_t kReturnValueFromFooV2 = 7;

  this->Needed({kFooV2DepFile});

  auto global_foo_v2_open = this->DlOpen(kFooV2DepFile.c_str(), RTLD_NOW | RTLD_GLOBAL);
  ASSERT_TRUE(global_foo_v2_open.is_ok()) << global_foo_v2_open.error_value();
  EXPECT_TRUE(global_foo_v2_open.value());

  this->Needed({kFooV1DepFile});

  auto global_foo_v1_open = this->DlOpen(kFooV1DepFile.c_str(), RTLD_NOW | RTLD_GLOBAL);
  ASSERT_TRUE(global_foo_v1_open.is_ok()) << global_foo_v1_open.error_value();
  EXPECT_TRUE(global_foo_v1_open.value());

  auto global_foo_v2_open_again =
      this->DlOpen(kFooV2DepFile.c_str(), RTLD_NOW | RTLD_GLOBAL | RTLD_NOLOAD);
  ASSERT_TRUE(global_foo_v2_open_again.is_ok()) << global_foo_v2_open_again.error_value();
  EXPECT_TRUE(global_foo_v2_open_again.value());

  EXPECT_EQ(global_foo_v2_open.value(), global_foo_v2_open_again.value());

  this->ExpectRootModule(kBarV2File);

  auto local_bar_v1_open = this->DlOpen(kBarV2File.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(local_bar_v1_open.is_ok()) << local_bar_v1_open.error_value();
  EXPECT_TRUE(local_bar_v1_open.value());

  auto local_bar_v1 = this->DlSym(local_bar_v1_open.value(), TestSym("bar_v1").c_str());
  ASSERT_TRUE(local_bar_v1.is_ok()) << local_bar_v1.error_value();
  EXPECT_TRUE(local_bar_v1.value());

  EXPECT_EQ(RunFunction<int64_t>(local_bar_v1.value()), kReturnValueFromFooV2);

  ASSERT_TRUE(this->DlClose(global_foo_v2_open.value()).is_ok());
  ASSERT_TRUE(this->DlClose(global_foo_v1_open.value()).is_ok());
  ASSERT_TRUE(this->DlClose(global_foo_v2_open_again.value()).is_ok());
  ASSERT_TRUE(this->DlClose(local_bar_v1_open.value()).is_ok());
}

// This tests that calling dlopen(..., RTLD_GLOBAL) with a previously loaded
// global dependency will not change the "global load order"
// dlopen RTLD_GLOBAL foo-v2 -> foo() returns 7
// dlopen RTLD_GLOBAL multiple-foo-deps:
//    - foo-v1 -> foo() returns 2
//    - foo-v2 -> foo() returns 7
// dlopen bar-v1 -> bar_v1() calls foo().
//    - foo-v1 -> foo() returns 2
// Call foo() from bar-v1 and get 7 from global foo-v2, because foo-v2 was
// loaded first with RTLD_GLOBAL, and its order did not change.
TYPED_TEST(DlTests, GlobalModuleOrderingOfDeps) {
  const std::string kFooV2DepFile = TestShlib("libld-dep-foo-v2");
  const std::string kParentFile = TestModule("multiple-foo-deps");
  const std::string kFooV1DepFile = TestShlib("libld-dep-foo-v1");
  const std::string kBarV2File = TestShlib("libbar-v1");
  constexpr int64_t kReturnValueFromFooV2 = 7;

  this->Needed({kFooV2DepFile});

  auto global_foo_v2_open = this->DlOpen(kFooV2DepFile.c_str(), RTLD_NOW | RTLD_GLOBAL);
  ASSERT_TRUE(global_foo_v2_open.is_ok()) << global_foo_v2_open.error_value();
  EXPECT_TRUE(global_foo_v2_open.value());

  this->ExpectRootModule(kParentFile);
  this->Needed({kFooV1DepFile});

  auto global_parent_open = this->DlOpen(kParentFile.c_str(), RTLD_NOW | RTLD_GLOBAL);
  ASSERT_TRUE(global_parent_open.is_ok()) << global_parent_open.error_value();
  EXPECT_TRUE(global_parent_open.value());

  auto global_parent = this->DlSym(global_parent_open.value(), TestSym("call_foo").c_str());
  ASSERT_TRUE(global_parent.is_ok()) << global_parent.error_value();
  EXPECT_TRUE(global_parent.value());

  // Validity check foo() return value from first loaded (global) foo-v2.
  EXPECT_EQ(RunFunction<int64_t>(global_parent.value()), kReturnValueFromFooV2);

  this->ExpectRootModule(kBarV2File);

  auto local_bar_v1_open = this->DlOpen(kBarV2File.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(local_bar_v1_open.is_ok()) << local_bar_v1_open.error_value();
  EXPECT_TRUE(local_bar_v1_open.value());

  auto local_bar_v1 = this->DlSym(local_bar_v1_open.value(), TestSym("bar_v1").c_str());
  ASSERT_TRUE(local_bar_v1.is_ok()) << local_bar_v1.error_value();
  EXPECT_TRUE(local_bar_v1.value());

  EXPECT_EQ(RunFunction<int64_t>(local_bar_v1.value()), kReturnValueFromFooV2);

  ASSERT_TRUE(this->DlClose(global_foo_v2_open.value()).is_ok());
  ASSERT_TRUE(this->DlClose(global_parent_open.value()).is_ok());
  ASSERT_TRUE(this->DlClose(local_bar_v1_open.value()).is_ok());
}

// TODO(https://fxbug.dev/338233824): Add more global module tests:
// - Test that promoting only the dependency of module to global does not affect
// the root module's visibility.
// dlopen has-bar-v1:
//   - bar-v1 -> calls foo(), defines bar() -> bar() returns 9
//      - foo-v1 -> foo() returns 2
// dlopen RTLD_GLOBAL foo-v1
// dlopen has-bar-v2:
//   - bar-v2 -> calls foo(), defines bar() -> bar() returns 3
//      - foo-v2 -> foo() returns 7
// call foo() from has-bar-v2 and get 2 from global foo-v1. call bar() from
// has-bar-v2 and get 3 from the dependency bar-v2.

// TODO(https://fxbug.dev/374375563)
// - Test that a module and its dependencies remain a global module after they
// have been promoted.
// dlopen RTLD_GLOBAL has-foo-v1:
//    - foo-v1 -> foo() returns 2
// dlopen RTLD_LOCAL foo-v1
// dlopen RTLD_LOCAL has-foo-v2:
//  - foo-v2 -> foo() returns 7
// call foo() from has-foo-v2 and expect 2 from the still global foo-v1.
// dlclose has-foo-v1
// dlopen bar-v2 -> calls foo()
//   - foo-v2 -> foo() returns 7
// Call bar() from bar-v2, and it should return 2 from the still global foo-v1.

// TODO(https://fxbug.dev/362604713)
// - Test applying RTLD_GLOBAL to a node along the circular dependency chain.

// Test startup modules are global modules managed by the dynamic linker
// - (startup module) foo-v1 -> foo() returns 2
// - (startup module) foo-v2 -> foo() returns 7
// dlopen(foo-v1, RTLD_NOLOAD) and expect the module to be loaded.
// dlopen(foo-v2, RTLD_NOLOAD) and expect the module to be loaded.
// dlopen has-foo-v2:
//  - foo-v2 -> foo() returns 7
// call foo from has-foo-v2 and expect 2 from global startup module that was
// loaded first (foo-v1).
TYPED_TEST(DlTests, StartupModulesBasic) {
  const std::string kFooV1File = TestShlib("libld-dep-foo-v1");
  const std::string kFooV2File = TestShlib("libld-dep-foo-v2");
  const std::string kHasFooV2File = TestShlib("libhas-foo-v2");
  constexpr int64_t kReturnValueFromFooV1 = 2;
  constexpr int64_t kReturnValueFromFooV2 = 7;

  // Make sure foo-v1, foo-v2 are linked in with this test by making direct
  // calls to their unique symbols.
  EXPECT_EQ(foo_v1_StartupModulesBasic(), kReturnValueFromFooV1);
  EXPECT_EQ(foo_v2_StartupModulesBasic(), kReturnValueFromFooV2);

  auto foo_v1_open = this->DlOpen(kFooV1File.c_str(), RTLD_NOW | RTLD_LOCAL | RTLD_NOLOAD);
  ASSERT_TRUE(foo_v1_open.is_ok()) << foo_v1_open.error_value();
  EXPECT_TRUE(foo_v1_open.value());

  auto foo_v2_open = this->DlOpen(kFooV2File.c_str(), RTLD_NOW | RTLD_LOCAL | RTLD_NOLOAD);
  ASSERT_TRUE(foo_v2_open.is_ok()) << foo_v2_open.error_value();
  EXPECT_TRUE(foo_v2_open.value());

  this->Needed({kHasFooV2File});

  auto has_foov2_open = this->DlOpen(kHasFooV2File.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(has_foov2_open.is_ok()) << has_foov2_open.error_value();
  EXPECT_TRUE(has_foov2_open.value());

  auto has_foo_v2 = this->DlSym(has_foov2_open.value(), TestSym("call_foo").c_str());
  ASSERT_TRUE(has_foo_v2.is_ok()) << has_foo_v2.error_value();
  ASSERT_TRUE(has_foo_v2.value());

  EXPECT_EQ(RunFunction<int64_t>(has_foo_v2.value()), kReturnValueFromFooV1);

  ASSERT_TRUE(this->DlClose(foo_v1_open.value()).is_ok());
  ASSERT_TRUE(this->DlClose(foo_v2_open.value()).is_ok());
  ASSERT_TRUE(this->DlClose(has_foov2_open.value()).is_ok());
}

// Test that global modules that are dlopen-ed are ordered after startup modules.
// (startup module) has-foo-v1:
//     - foo-v1 -> foo() returns 2
// dlopen RTLD_GLOBAL has-foo-v2:
//  - foo-v2 -> foo() returns 7
// call foo from has-foo-v2 and expect 2 from global startup module that was
// loaded first (foo-v1).
TYPED_TEST(DlTests, StartupModulesPriorityOverGlobal) {
  const std::string kHasFooV1File = TestShlib("libhas-foo-v1");
  const std::string kHasFooV2File = TestShlib("libhas-foo-v2");
  const std::string kFooV2DepFile = TestShlib("libld-dep-foo-v2");
  constexpr int64_t kReturnValueFromFooV1 = 2;

  // Make sure has-foo-v1 is linked in with this test by making a direct call to
  // its unique symbol.
  EXPECT_EQ(call_foo_v1_StartupModulesPriorityOverGlobal(), kReturnValueFromFooV1);

  auto startup_parent_open =
      this->DlOpen(kHasFooV1File.c_str(), RTLD_NOW | RTLD_LOCAL | RTLD_NOLOAD);
  ASSERT_TRUE(startup_parent_open.is_ok()) << startup_parent_open.error_value();
  EXPECT_TRUE(startup_parent_open.value());

  this->Needed({kHasFooV2File, kFooV2DepFile});

  auto has_foov2_open = this->DlOpen(kHasFooV2File.c_str(), RTLD_NOW | RTLD_GLOBAL);
  ASSERT_TRUE(has_foov2_open.is_ok()) << has_foov2_open.error_value();
  EXPECT_TRUE(has_foov2_open.value());

  auto has_foo_v2 = this->DlSym(has_foov2_open.value(), TestSym("call_foo").c_str());
  ASSERT_TRUE(has_foo_v2.is_ok()) << has_foo_v2.error_value();
  ASSERT_TRUE(has_foo_v2.value());

  EXPECT_EQ(RunFunction<int64_t>(has_foo_v2.value()), kReturnValueFromFooV1);

  ASSERT_TRUE(this->DlClose(startup_parent_open.value()).is_ok());
  ASSERT_TRUE(this->DlClose(has_foov2_open.value()).is_ok());
}

TYPED_TEST(DlTests, StartupModulesStaticTlsDesc) {
  const std::string kGetTlsVarFile = "static-tls-desc-module.so";

  EXPECT_EQ(gStaticTlsVar, kStaticTlsDataValue);

  this->ExpectRootModule(kGetTlsVarFile);

  auto open = this->DlOpen(kGetTlsVarFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(open.is_ok()) << open.error_value();
  EXPECT_TRUE(open.value()) << open.error_value();

  auto sym = this->DlSym(open.value(), "get_static_tls_var");
  ASSERT_TRUE(sym.is_ok()) << sym.error_value();
  ASSERT_TRUE(sym.value());

  int* ptr = RunFunction<int*>(sym.value());
  EXPECT_EQ(*ptr, kStaticTlsDataValue);

  ASSERT_TRUE(this->DlClose(open.value()).is_ok());
}

TYPED_TEST(DlTests, StartupModulesStaticTlsGetAddr) {
  const std::string kGetTlsVarFile = "static-tls-module.so";

  this->ExpectRootModule(kGetTlsVarFile);

  // Don't expect tls_get_addr() to return any useful value for relocations, but
  // expect that dlopen() will at least succeed when calling it.
  auto open = this->DlOpen(kGetTlsVarFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(open.is_ok()) << open.error_value();
  EXPECT_TRUE(open.value()) << open.error_value();

  ASSERT_TRUE(this->DlClose(open.value()).is_ok());
}

// dlopen a module whose initializers and finalizers are decoded by legacy
// DT_INIT and DT_FINI sections. These functions will update the global
// gInitFiniState, and that value is checked in this test to ensure those
// functions were run.
TYPED_TEST(DlTests, InitFiniLegacy) {
  const std::string kFile = TestModule("init-fini-legacy");

  if constexpr (!TestFixture::kSupportsInitFini) {
    GTEST_SKIP() << "test requires init/fini support";
  }

  ASSERT_EQ(gInitFiniState, 0);

  this->ExpectRootModule(kFile);

  auto open = this->DlOpen(kFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(open.is_ok()) << open.error_value();
  EXPECT_TRUE(open.value()) << open.error_value();

  EXPECT_EQ(gInitFiniState, 101);

  ASSERT_TRUE(this->DlClose(open.value()).is_ok());

  if (TestFixture::kDlCloseCanRunFinalizers) {
    EXPECT_EQ(gInitFiniState, 102);
  }

  gInitFiniState = 0;
}

// Similar to InitFiniLegacy test, except dlopen a module whose initializers and
// finalizers are decoded from DT_INIT_ARRAY/DT_FINI_ARRAY sections. This also
// tests that multiple initializers/finalizers in the dlopen-ed module are run in
// correct order.
TYPED_TEST(DlTests, InitFiniArray) {
  const std::string kFile = TestModule("init-fini-array");

  if constexpr (!TestFixture::kSupportsInitFini) {
    GTEST_SKIP() << "test requires init/fini support";
  }

  ASSERT_EQ(gInitFiniState, 0);

  this->ExpectRootModule(kFile);

  auto open = this->DlOpen(kFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(open.is_ok()) << open.error_value();
  EXPECT_TRUE(open.value()) << open.error_value();

  // Expect the three ctors to have run in the order expected by the functions
  // in init-fini-array.cc
  EXPECT_EQ(gInitFiniState, 3);

  ASSERT_TRUE(this->DlClose(open.value()).is_ok());

  if (TestFixture::kDlCloseCanRunFinalizers) {
    // Expect the three dtors to have run in the order expected by the functions
    // in init-fini-array.cc
    EXPECT_EQ(gInitFiniState, 6);
  }

  gInitFiniState = 0;
}

// A common test subroutine for basic TLS accesses.
//
// This test exercises the following sequence of events:
//   1. The initial thread is created with initial-exec TLS state.
//   2. dlopen adds dynamic TLS state, and bumps DTV generation.
//   3. The initial thread uses dynamic TLS via the new DTV.
//   4. New threads are launched.
//   5. The new threads use dynamic TLS via their initial DTV (i.e., fast path).
//
// NOTE: Whether the slow path may be used in this test depends on the
// implementation. For instance, at the time of writing, musl's dlopen doesn't
// update the calling thread's DTV and instead relies on the first access on the
// thread to use the slow path to call __tls_get_new. However, this test should
// only be relied upon for testing the fast path, because that is the only thing
// we can guarantee for all implementations.
template <class TestFixture, bool UseTlsDesc, class Test>
void BasicGlobalDynamicTls(Test& self) {
  if constexpr (!TestFixture::kSupportsDynamicTls) {
    GTEST_SKIP() << "test requires TLS";
  }

  constexpr const char* kTlsModuleName =
      UseTlsDesc ? "tls-desc-dep-module.so" : "tls-dep-module.so";
  constexpr const char* kGetTlsDepDataPtr = "get_tls_dep_data";
  constexpr const char* kGetTlsDescDepBss1 = "get_tls_dep_bss1";
  constexpr const char* kGetTlsDescDepWeak = "get_tls_dep_weak";

  // Initial values for TLS variables in the test module.
  constexpr int kTlsDataInitialVal = 42;
  constexpr char kBss1InitialVal = 0;

  self.ExpectRootModule(kTlsModuleName);

  // The module should exist for both tls-dep and tls-desc-dep targets.
  auto result = self.DlOpen(kTlsModuleName, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_ok()) << result.error_value();
  EXPECT_TRUE(result.value());

  // The module should exist for both tls-dep and tls-desc-dep targets, but if
  // it wasn't compiled to have the right type of TLS relocations, then the
  // symbols won't exist in the module, and we should skip the rest of the
  // test.
  auto get_dep_data = self.DlSym(result.value(), kGetTlsDepDataPtr);
  if (get_dep_data.is_error()) {
    EXPECT_THAT(get_dep_data.error_value().take_str(),
                IsUndefinedSymbolErrMsg(kGetTlsDepDataPtr, kTlsModuleName));
    auto close_result = self.DlClose(result.value());
    ASSERT_TRUE(close_result.is_ok()) << close_result.error_value();
    GTEST_SKIP() << "Test module disabled at compile time.";
  }

  ASSERT_TRUE(get_dep_data.value());

  int* data_ptr1 = RunFunction<int*>(get_dep_data.value());
  ASSERT_TRUE(data_ptr1);

  int* data_ptr2 = RunFunction<int*>(get_dep_data.value());
  ASSERT_TRUE(data_ptr2);

  // data_ptr1 and data_ptr2 should alias, since `get_tls_dep_data` returns a
  // pointer to the thread local. This can fail if DTP_OFFSET is non-zero and
  // the arithmetic using it does not get applied uniformly, causing the
  // returned pointer to be different than the one stored in the GOT for the
  // TLSDESC fast path.
  EXPECT_EQ(data_ptr1, data_ptr2);
  EXPECT_EQ(*data_ptr1, kTlsDataInitialVal);
  EXPECT_EQ(*data_ptr1, *data_ptr2);

  // Modifying the thread local value should not impact other threads. We test
  // this in the loop below.
  *data_ptr1 += 1;
  EXPECT_EQ(*data_ptr1, kTlsDataInitialVal + 1);
  EXPECT_EQ(data_ptr1, data_ptr2);

  auto get_dep_bss1 = self.DlSym(result.value(), kGetTlsDescDepBss1);
  ASSERT_TRUE(get_dep_bss1.is_ok()) << get_dep_bss1.error_value();
  ASSERT_TRUE(get_dep_bss1.value());

  char* bss1_ptr = RunFunction<char*>(get_dep_bss1.value());
  ASSERT_TRUE(bss1_ptr);
  EXPECT_EQ(*bss1_ptr, kBss1InitialVal);
  *bss1_ptr = 1;
  EXPECT_EQ(*RunFunction<char*>(get_dep_bss1.value()), 1);

  auto get_dep_weak = self.DlSym(result.value(), kGetTlsDescDepWeak);
  ASSERT_TRUE(get_dep_weak.is_ok()) << get_dep_weak.error_value();
  ASSERT_TRUE(get_dep_weak.value());
  if constexpr (UseTlsDesc) {
    // We can only be sure the returned value will be nullptr w/ TLSDESC.
    // __tls_get_addr may just return whatever happens to be in the GOT.
    int* weak_ptr = RunFunction<int*>(get_dep_weak.value());
    EXPECT_EQ(weak_ptr, nullptr);
  }

  // The value of another threads TLS variable should be unaffected by this
  // thread.
  constexpr int kNumThreads = 10;
  std::vector<std::thread> threads;
  threads.reserve(kNumThreads);
  for (int i = 0; i < kNumThreads; ++i) {
    threads.emplace_back([&get_dep_data, &get_dep_bss1, kTlsDataInitialVal, kBss1InitialVal] {
      // Check that each TLS value has its initial value.
      EXPECT_EQ(*RunFunction<int*>(get_dep_data.value()), kTlsDataInitialVal);
      EXPECT_EQ(*RunFunction<char*>(get_dep_bss1.value()), kBss1InitialVal);
    });
  }
  for (auto& thread : threads) {
    thread.join();
  }
  ASSERT_TRUE(self.DlClose(result.value()).is_ok());
}

TYPED_TEST(DlTests, BasicGlobalDynamicTlsDesc) { BasicGlobalDynamicTls<TestFixture, true>(*this); }

TYPED_TEST(DlTests, BasicGlobalDynamicTlsGetAddr) {
  BasicGlobalDynamicTls<TestFixture, false>(*this);
}

}  // namespace
