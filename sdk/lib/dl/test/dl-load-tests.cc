// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <thread>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "dl-impl-tests.h"
#include "dl-system-tests.h"

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
  if constexpr (TestFixture::kCanMatchExactError) {
    EXPECT_EQ(result.error_value().take_str(),
              "missing-sym.MissingSymbol.module.so: undefined symbol: missing_sym_MissingSymbol");
  } else {
    EXPECT_THAT(
        result.error_value().take_str(),
        MatchesRegex(
            // emitted by Fuchsia-musl
            "Error relocating missing-sym.MissingSymbol.module.so: missing_sym_MissingSymbol: symbol not found"
            // emitted by Linux-glibc
            "|.*missing-sym.MissingSymbol.module.so: undefined symbol: missing_sym_MissingSymbol"));
  }
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

  if constexpr (!TestFixture::kCanReuseLoadedDeps) {
    GTEST_SKIP() << "test requires that fixture can reuse loaded modules for dependencies";
  }

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

  if constexpr (!TestFixture::kCanReuseLoadedDeps) {
    GTEST_SKIP() << "test requires that fixture can reuse loaded modules for dependencies";
  }

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

  if (!TestFixture::kCanReuseLoadedDeps) {
    GTEST_SKIP() << "test requires that fixture can reuse loaded dependencies";
  }

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

  if constexpr (!TestFixture::kCanReuseLoadedDeps) {
    GTEST_SKIP() << "test requires that fixture can reuse loaded modules for dependencies";
  }

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

  if constexpr (TestFixture::kDlSymSupportsDeps) {
    auto bar_v1 = this->DlSym(res1.value(), TestSym("bar_v1").c_str());
    ASSERT_TRUE(bar_v1.is_ok()) << bar_v1.error_value();
    ASSERT_TRUE(bar_v1.value());

    EXPECT_EQ(RunFunction<int64_t>(bar_v1.value()), kReturnValueFromFooV1);

    auto bar_v2 = this->DlSym(res1.value(), TestSym("bar_v2").c_str());
    ASSERT_TRUE(bar_v2.is_ok()) << bar_v2.error_value();
    ASSERT_TRUE(bar_v2.value());

    EXPECT_EQ(RunFunction<int64_t>(bar_v2.value()), kReturnValueFromFooV1);
  }

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

  if constexpr (TestFixture::kDlSymSupportsDeps) {
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
  }

  ASSERT_TRUE(this->DlClose(res1.value()).is_ok());
}

// These are test scenarios that test symbol resolution with RTLD_GLOBAL.

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

  if constexpr (!TestFixture::kSupportsGlobalMode) {
    GTEST_SKIP() << "test requires that fixture supports RTLD_GLOBAL";
  }

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

  if constexpr (!TestFixture::kSupportsGlobalMode) {
    GTEST_SKIP() << "test requires that fixture supports RTLD_GLOBAL";
  }

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

  if constexpr (!TestFixture::kSupportsGlobalMode) {
    GTEST_SKIP() << "test requires that fixture supports RTLD_GLOBAL";
  }

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

  // dlsym will not be able to find the global symbol from the local scope
  auto sym3 = this->DlSym(res2.value(), TestSym("missing_sym").c_str());
  ASSERT_TRUE(sym3.is_error()) << sym3.error_value();
  if constexpr (TestFixture::kCanMatchExactError || TestFixture::kEmitsSymbolNotFound) {
    EXPECT_EQ(sym3.error_value().take_str(),
              "Symbol not found: missing_sym_GlobalSatisfiesMissingSymbol");
  } else {
    // emitted by Linux-glibc
    EXPECT_THAT(
        sym3.error_value().take_str(),
        // only the module name is captured from the full test path
        MatchesRegex(
            ".*missing-sym.GlobalSatisfiesMissingSymbol.module.so: undefined symbol: missing_sym_GlobalSatisfiesMissingSymbol"));
  }

  ASSERT_TRUE(this->DlClose(res1.value()).is_ok());
  ASSERT_TRUE(this->DlClose(res2.value()).is_ok());
}

// TODO(https://fxbug.dev/338232267)
// Test that changing mode to RTLD_GLOBAL will change how a symbol is resolved
// by subsequent modules.
// dlopen foo-v1 -> foo() returns 2
// dlopen RTLD_GLOBAL foo-v2 -> foo() returns 7
// dlopen has-foo-v1
//   - foo-v1 -> foo() returns 2
// call foo() from has-foo-v1 and expect 2 from previously loaded global foo-v2
// dlopen RTLD_GLOBAL foo-v1
// call foo() from has-foo-v1 and still expect 2 because it does not get
// re-resolved.
// dlopen has-foo-v2
//  - foo-v2 -> foo() returns 7
// call foo() from has-foo-v2 and expect 2 from foo-v1, because foo-v1 is now
// the first loaded global module with the symbol.

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
template <class TestFixture, class Test>
void BasicGlobalDynamicTls(Test& self, const char* module_name) {
  if constexpr (!TestFixture::kSupportsTls) {
    GTEST_SKIP() << "test requires TLS";
  }

  constexpr const char* kGetTlsDescDepDataPtr = "get_tls_dep_data";
  constexpr const char* kGetTlsDescDepBss1 = "get_tls_dep_bss1";
  constexpr const char* kGetTlsDescDepWeak = "get_tls_dep_weak";

  // The module should exist for both tls-dep and tls-desc-dep targets.
  auto result = self.DlOpen(module_name, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_ok()) << result.error_value();
  EXPECT_TRUE(result.value());

  // The module should exist for both tls-dep and tls-desc-dep targets, but if
  // it wasn't compiled to have the right type of TLS relocations, then the
  // symbols won't exist in the module, and we should skip the rest of the
  // test.
  auto get_dep_data = self.DlSym(result.value(), kGetTlsDescDepDataPtr);
  if (get_dep_data.is_error()) {
    ASSERT_THAT(get_dep_data.error_value().take_str(),
                ::testing::EndsWith(std::string("undefined symbol: ") + kGetTlsDescDepDataPtr));
    auto close_result = self.DlClose(result.value());
    ASSERT_TRUE(close_result.is_ok()) << close_result.error_value();
    GTEST_SKIP() << "Test module disabled at compile time.";
  }

  ASSERT_TRUE(get_dep_data.value());

  int* data_ptr1 = RunFunction<int*>(get_dep_data.value());
  ASSERT_TRUE(data_ptr1);

  int* data_ptr2 = RunFunction<int*>(get_dep_data.value());
  ASSERT_TRUE(data_ptr2);

  constexpr int kTlsDataInitialVal = 42;
  constexpr char kBss1InitialVal = 0;

  EXPECT_EQ(*data_ptr1, kTlsDataInitialVal);
  EXPECT_EQ(*data_ptr1, *data_ptr2);

  // data_ptr1 and data_ptr2 should alias, since `get_tls_dep_data` returns a
  // pointer to the thread local. This can fail if DTP_OFFSET is non-zero and
  // the arithmetic using it does not get applied uniformly, causing the
  // returned pointer to be different than the one stored in the GOT for the
  // TLSDESC fast path.
  EXPECT_EQ(data_ptr1, data_ptr2);  // Fails on RISC-V, data_ptr2 == data_ptr1 + kRISCVDtpOffset

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
#if HAVE_TLSDESC
  // We can only be sure the returned value will be nullptr w/ TLSDESC.
  // __tls_get_addr may just return whatever happens to be in the GOT.
  int* weak_ptr = RunFunction<int*>(get_dep_weak.value());
  EXPECT_EQ(weak_ptr, nullptr);
#endif

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

TYPED_TEST(DlTests, BasicGlobalDynamicTlsDesc) {
  BasicGlobalDynamicTls<TestFixture>(*this, "tls-desc-dep-module.so");
}

TYPED_TEST(DlTests, BasicGlobalDynamicTlsGetAddr) {
  BasicGlobalDynamicTls<TestFixture>(*this, "tls-dep-module.so");
}

}  // namespace
