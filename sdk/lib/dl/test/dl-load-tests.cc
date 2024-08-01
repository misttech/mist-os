// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

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
  constexpr const char* kFile = "does-not-exist.so";

  this->ExpectMissing(kFile);

  auto result = this->DlOpen(kFile, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_error());
  if constexpr (TestFixture::kCanMatchExactError) {
    EXPECT_EQ(result.error_value().take_str(), "does-not-exist.so not found");
  } else {
    EXPECT_THAT(
        result.error_value().take_str(),
        MatchesRegex(
            // emitted by Fuchsia-musl
            "Error loading shared library .*does-not-exist.so: ZX_ERR_NOT_FOUND"
            // emitted by Linux-glibc
            "|.*does-not-exist.so: cannot open shared object file: No such file or directory"));
  }
}

TYPED_TEST(DlTests, InvalidMode) {
  constexpr const char* kFile = "ret17.module.so";

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

  auto result = this->DlOpen(kFile, bad_mode);
  ASSERT_TRUE(result.is_error());
  EXPECT_EQ(result.error_value().take_str(), "invalid mode parameter")
      << "for mode argument " << bad_mode;
}

// Load a basic file with no dependencies.
TYPED_TEST(DlTests, Basic) {
  constexpr int64_t kReturnValue = 17;
  constexpr const char* kFile = "ret17.module.so";

  // TODO(https://fxbug.dev/354043838): Fold into ExpectRootModule/Needed API.
  this->ExpectRootModuleNotLoaded(kFile);

  this->ExpectRootModule(kFile);

  auto result = this->DlOpen(kFile, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_ok()) << result.error_value();
  EXPECT_TRUE(result.value());

  // Look up the "TestStart" function and call it, expecting it to return 17.
  auto sym_result = this->DlSym(result.value(), "TestStart");
  ASSERT_TRUE(sym_result.is_ok()) << result.error_value();
  ASSERT_TRUE(sym_result.value());

  EXPECT_EQ(RunFunction<int64_t>(sym_result.value()), kReturnValue);

  ASSERT_TRUE(this->DlClose(result.value()).is_ok());
}

// Load a file that performs relative relocations against itself. The TestStart
// function's return value is derived from the resolved symbols.
TYPED_TEST(DlTests, Relative) {
  constexpr int64_t kReturnValue = 17;
  constexpr const char* kFile = "relative-reloc.module.so";

  // TODO(https://fxbug.dev/354043838): Fold into ExpectRootModule/Needed API.
  this->ExpectRootModuleNotLoaded(kFile);

  this->ExpectRootModule(kFile);

  auto result = this->DlOpen(kFile, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_ok()) << result.error_value();
  EXPECT_TRUE(result.value());

  auto sym_result = this->DlSym(result.value(), "TestStart");
  ASSERT_TRUE(sym_result.is_ok()) << result.error_value();
  ASSERT_TRUE(sym_result.value());

  EXPECT_EQ(RunFunction<int64_t>(sym_result.value()), kReturnValue);

  ASSERT_TRUE(this->DlClose(result.value()).is_ok());
}

// Load a file that performs symbolic relocations against itself. The TestStart
// functions' return value is derived from the resolved symbols.
TYPED_TEST(DlTests, Symbolic) {
  constexpr int64_t kReturnValue = 17;
  constexpr const char* kFile = "symbolic-reloc.module.so";

  // TODO(https://fxbug.dev/354043838): Fold into ExpectRootModule/Needed API.
  this->ExpectRootModuleNotLoaded(kFile);

  this->ExpectRootModule(kFile);

  auto result = this->DlOpen(kFile, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_ok()) << result.error_value();
  EXPECT_TRUE(result.value());

  auto sym_result = this->DlSym(result.value(), "TestStart");
  ASSERT_TRUE(sym_result.is_ok()) << result.error_value();
  ASSERT_TRUE(sym_result.value());

  EXPECT_EQ(RunFunction<int64_t>(sym_result.value()), kReturnValue);

  ASSERT_TRUE(this->DlClose(result.value()).is_ok());
}

// Load a module that depends on a symbol provided directly by a dependency.
TYPED_TEST(DlTests, BasicDep) {
  constexpr int64_t kReturnValue = 17;
  constexpr const char* kFile = "basic-dep.module.so";
  constexpr const char* kDepFile = "libld-dep-a.so";

  // TODO(https://fxbug.dev/354043838): Fold into ExpectRootModule/Needed API.
  this->ExpectRootModuleNotLoaded(kFile);
  this->ExpectNeededNotLoaded({kDepFile});

  this->ExpectRootModule(kFile);
  this->Needed({kDepFile});

  auto result = this->DlOpen(kFile, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_ok()) << result.error_value();
  EXPECT_TRUE(result.value());

  auto sym_result = this->DlSym(result.value(), "TestStart");
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
  constexpr const char* kFile = "indirect-deps.module.so";
  constexpr const char* kDepFile1 = "libindirect-deps-a.so";
  constexpr const char* kDepFile2 = "libindirect-deps-b.so";
  constexpr const char* kDepFile3 = "libindirect-deps-c.so";

  // TODO(https://fxbug.dev/354043838): Fold into ExpectRootModule/Needed API.
  this->ExpectRootModuleNotLoaded(kFile);
  this->ExpectNeededNotLoaded({kDepFile1, kDepFile2, kDepFile3});

  this->ExpectRootModule(kFile);
  this->Needed({kDepFile1, kDepFile2, kDepFile3});

  auto result = this->DlOpen(kFile, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_ok()) << result.error_value();
  EXPECT_TRUE(result.value());

  auto sym_result = this->DlSym(result.value(), "TestStart");
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
  constexpr const char* kFile = "many-deps.module.so";
  constexpr const char* kDepFile1 = "libld-dep-a.so";
  constexpr const char* kDepFile2 = "libld-dep-b.so";
  constexpr const char* kDepFile3 = "libld-dep-f.so";
  constexpr const char* kDepFile4 = "libld-dep-c.so";
  constexpr const char* kDepFile5 = "libld-dep-d.so";
  constexpr const char* kDepFile6 = "libld-dep-e.so";

  // TODO(https://fxbug.dev/354043838): Fold into ExpectRootModule/Needed API.
  this->ExpectRootModuleNotLoaded(kFile);
  this->ExpectNeededNotLoaded({kDepFile1, kDepFile2, kDepFile3, kDepFile4, kDepFile5, kDepFile6});

  this->ExpectRootModule(kFile);
  this->Needed({kDepFile1, kDepFile2, kDepFile3, kDepFile4, kDepFile5, kDepFile6});

  auto result = this->DlOpen(kFile, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_ok()) << result.error_value();
  EXPECT_TRUE(result.value());

  auto sym_result = this->DlSym(result.value(), "TestStart");
  ASSERT_TRUE(sym_result.is_ok()) << result.error_value();
  ASSERT_TRUE(sym_result.value());

  EXPECT_EQ(RunFunction<int64_t>(sym_result.value()), kReturnValue);

  ASSERT_TRUE(this->DlClose(result.value()).is_ok());
}

// TODO(https://fxbug.dev/339028040): Test missing symbol in transitive dep.
// Load a module that depends on libld-dep-a.so, but this dependency does not
// provide the b symbol referenced by the root module, so relocation fails.
TYPED_TEST(DlTests, MissingSymbol) {
  constexpr const char* kFile = "missing-sym.module.so";
  constexpr const char* kDepFile = "libld-dep-a.so";

  // TODO(https://fxbug.dev/354043838): Fold into ExpectRootModule/Needed API.
  this->ExpectRootModuleNotLoaded(kFile);
  this->ExpectNeededNotLoaded({kDepFile});

  this->ExpectRootModule(kFile);
  this->Needed({kDepFile});

  auto result = this->DlOpen(kFile, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_error());
  if constexpr (TestFixture::kCanMatchExactError) {
    EXPECT_EQ(result.error_value().take_str(), "missing-sym.module.so: undefined symbol: c");
  } else {
    EXPECT_THAT(result.error_value().take_str(),
                MatchesRegex(
                    // emitted by Fuchsia-musl
                    "Error relocating missing-sym.module.so: c: symbol not found"
                    // emitted by Linux-glibc
                    "|.*missing-sym.module.so: undefined symbol: c"));
  }
}

// TODO(https://fxbug.dev/3313662773): Test simple case of transitive missing
// symbol.
// dlopen missing-transitive-symbol:
//  - missing-transitive-sym
//    - has-missing-sym is missing a()
// call a() from missing-transitive-symbol, and expect symbol not found

// Try to load a module that has a (direct) dependency that cannot be found.
TYPED_TEST(DlTests, MissingDependency) {
  constexpr const char* kFile = "missing-dep.module.so";
  constexpr const char* kDepFile = "libmissing-dep-dep.so";

  // TODO(https://fxbug.dev/354043838): Fold into ExpectRootModule/Needed API.
  this->ExpectRootModuleNotLoaded(kFile);

  this->ExpectRootModule(kFile);
  this->Needed({NotFound(kDepFile)});

  auto result = this->DlOpen(kFile, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_error());

  // TODO(https://fxbug.dev/336633049): Harmonize "not found" error messages
  // between implementations.
  // Expect that the dependency lib to missing-dep.module.so cannot be found.
  if constexpr (TestFixture::kCanMatchExactError) {
    EXPECT_EQ(result.error_value().take_str(), "cannot open dependency: libmissing-dep-dep.so");
  } else {
    EXPECT_THAT(
        result.error_value().take_str(),
        MatchesRegex(
            // emitted by Fuchsia-musl
            "Error loading shared library .*libmissing-dep-dep.so: ZX_ERR_NOT_FOUND \\(needed by missing-dep.module.so\\)"
            // emitted by Linux-glibc
            "|.*libmissing-dep-dep.so: cannot open shared object file: No such file or directory"));
  }
}

// Try to load a module where the dependency of its direct dependency (i.e. a
// transitive dependency of the root module) cannot be found.
TYPED_TEST(DlTests, MissingTransitiveDependency) {
  constexpr const char* kFile = "missing-transitive-dep.module.so";
  constexpr const char* kDepFile1 = "libhas-missing-dep.so";
  constexpr const char* kDepFile2 = "libmissing-dep-dep.so";

  // TODO(https://fxbug.dev/354043838): Fold into ExpectRootModule/Needed API.
  this->ExpectRootModuleNotLoaded(kFile);
  this->ExpectNeededNotLoaded({kDepFile1});

  this->ExpectRootModule(kFile);
  this->Needed({Found(kDepFile1), NotFound(kDepFile2)});

  auto result = this->DlOpen(kFile, RTLD_NOW | RTLD_LOCAL);
  // TODO(https://fxbug.dev/336633049): Harmonize "not found" error messages
  // between implementations.
  // Expect that the dependency lib to libhas-missing-dep.so cannot be found.
  if constexpr (TestFixture::kCanMatchExactError) {
    EXPECT_EQ(result.error_value().take_str(), "cannot open dependency: libmissing-dep-dep.so");
  } else {
    EXPECT_THAT(
        result.error_value().take_str(),
        MatchesRegex(
            // emitted by Fuchsia-musl
            "Error loading shared library .*libmissing-dep-dep.so: ZX_ERR_NOT_FOUND \\(needed by libhas-missing-dep.so\\)"
            // emitted by Linux-glibc
            "|.*libmissing-dep-dep.so: cannot open shared object file: No such file or directory"));
  }
}

// Test loading a module with relro protections.
TYPED_TEST(DlTests, Relro) {
  constexpr const char* kFile = "relro.module.so";

  // TODO(https://fxbug.dev/354043838): Fold into ExpectRootModule/Needed API.
  this->ExpectRootModuleNotLoaded(kFile);

  this->ExpectRootModule(kFile);

  auto result = this->DlOpen(kFile, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_ok()) << result.error_value();
  ASSERT_TRUE(result.value());

  auto sym = this->DlSym(result.value(), "TestStart");
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
  constexpr const char* kFile = "ret17.module.so";

  // TODO(https://fxbug.dev/354043838): Fold into ExpectRootModule/Needed API.
  this->ExpectRootModuleNotLoaded(kFile);

  this->ExpectRootModule(kFile);

  auto res1 = this->DlOpen(kFile, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res1.is_ok()) << res1.error_value();
  auto ptr1 = res1.value();
  EXPECT_TRUE(ptr1);

  auto res2 = this->DlOpen(kFile, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res2.is_ok()) << res2.error_value();
  auto ptr2 = res2.value();
  EXPECT_TRUE(ptr2);

  EXPECT_EQ(ptr1, ptr2);

  auto sym1 = this->DlSym(ptr1, "TestStart");
  ASSERT_TRUE(sym1.is_ok()) << sym1.error_value();
  auto sym1_ptr = sym1.value();
  EXPECT_TRUE(sym1_ptr);

  auto sym2 = this->DlSym(ptr2, "TestStart");
  ASSERT_TRUE(sym2.is_ok()) << sym2.error_value();
  auto sym2_ptr = sym2.value();
  EXPECT_TRUE(sym2_ptr);

  EXPECT_EQ(sym1_ptr, sym2_ptr);

  ASSERT_TRUE(this->DlClose(ptr1).is_ok());
  ASSERT_TRUE(this->DlClose(ptr2).is_ok());
}

// Test that different mutually-exclusive files that were dlopen-ed do not share
// pointers or resolved symbols.
TYPED_TEST(DlTests, UniqueModules) {
  constexpr const char* kFile1 = "ret17.module.so";
  constexpr const char* kFile2 = "ret23.module.so";

  // TODO(https://fxbug.dev/354043838): Fold into ExpectRootModule/Needed API.
  this->ExpectRootModuleNotLoaded(kFile1);
  this->ExpectRootModuleNotLoaded(kFile2);

  this->ExpectRootModule(kFile1);

  auto ret17 = this->DlOpen(kFile1, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(ret17.is_ok()) << ret17.error_value();
  auto ret17_ptr = ret17.value();
  EXPECT_TRUE(ret17_ptr);

  this->ExpectRootModule(kFile2);

  auto ret23 = this->DlOpen(kFile2, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(ret23.is_ok()) << ret23.error_value();
  auto ret23_ptr = ret23.value();
  EXPECT_TRUE(ret23_ptr);

  EXPECT_NE(ret17_ptr, ret23_ptr);

  auto sym17 = this->DlSym(ret17_ptr, "TestStart");
  ASSERT_TRUE(sym17.is_ok()) << sym17.error_value();
  auto sym17_ptr = sym17.value();
  EXPECT_TRUE(sym17_ptr);

  auto sym23 = this->DlSym(ret23_ptr, "TestStart");
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
  constexpr const char* kFile = "libhas-foo-v1.OpenDepDirectly.so";
  constexpr const char* kDepFile = "libld-dep-foo-v1.OpenDepDirectly.so";

  if constexpr (!TestFixture::kCanReuseLoadedDeps) {
    GTEST_SKIP() << "test requires that fixture can reuse loaded modules for dependencies";
  }

  // TODO(https://fxbug.dev/354043838): Fold into ExpectRootModule/Needed API.
  this->ExpectNeededNotLoaded({kFile, kDepFile});

  this->Needed({kFile, kDepFile});

  auto res1 = this->DlOpen(kFile, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res1.is_ok()) << res1.error_value();
  EXPECT_TRUE(res1.value());

  // dlopen kDepFile expecting it to already be loaded.
  auto res2 = this->DlOpen(kDepFile, RTLD_NOW | RTLD_LOCAL | RTLD_NOLOAD);
  ASSERT_TRUE(res2.is_ok()) << res2.error_value();
  EXPECT_TRUE(res2.value());

  // Test that dlsym will resolve the same symbol pointer from the shared
  // dependency between kFile (res1) and kDepFile (res2).
  auto sym1 = this->DlSym(res1.value(), "foo");
  ASSERT_TRUE(sym1.is_ok()) << sym1.error_value();
  ASSERT_TRUE(sym1.value());

  auto sym2 = this->DlSym(res2.value(), "foo");
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
  constexpr const char* kFile = "multiple-foo-deps.DepOrder.so";
  constexpr const char* kDepFile1 = "libld-dep-foo-v1.DepOrder.so";
  constexpr const char* kDepFile2 = "libld-dep-foo-v2.DepOrder.so";

  // TODO(https://fxbug.dev/354043838): Fold into ExpectRootModule/Needed API.
  this->ExpectRootModuleNotLoaded(kFile);
  this->ExpectNeededNotLoaded({kDepFile1, kDepFile2});

  this->ExpectRootModule(kFile);
  this->Needed({kDepFile1, kDepFile2});

  auto res = this->DlOpen(kFile, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res.is_ok()) << res.error_value();
  EXPECT_TRUE(res.value());

  auto sym = this->DlSym(res.value(), "call_foo");
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
  constexpr const char* kFile = "transitive-foo-dep.TransitiveDepOrder.so";
  constexpr const char* kDepFile1 = "libhas-foo-v1.TransitiveDepOrder.so";
  constexpr const char* kDepFile2 = "libld-dep-foo-v2.TransitiveDepOrder.so";
  constexpr const char* kDepFile3 = "libld-dep-foo-v1.TransitiveDepOrder.so";

  // TODO(https://fxbug.dev/354043838): Fold into ExpectRootModule/Needed API.
  this->ExpectRootModuleNotLoaded(kFile);
  this->ExpectNeededNotLoaded({kDepFile1, kDepFile2, kDepFile3});

  this->ExpectRootModule(kFile);
  this->Needed({kDepFile1, kDepFile2, kDepFile3});

  auto res = this->DlOpen(kFile, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res.is_ok()) << res.error_value();
  EXPECT_TRUE(res.value());

  auto sym = this->DlSym(res.value(), "call_foo");
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
  constexpr const char* kFile = "multiple-foo-deps.LocalPrecedence.so";
  constexpr const char* kDepFile1 = "libld-dep-foo-v1.LocalPrecedence.so";
  constexpr const char* kDepFile2 = "libld-dep-foo-v2.LocalPrecedence.so";
  constexpr int64_t kReturnValueFromFooV1 = 2;
  constexpr int64_t kReturnValueFromFooV2 = 7;

  if constexpr (!TestFixture::kCanReuseLoadedDeps) {
    GTEST_SKIP() << "test requires that fixture can reuse loaded modules for dependencies";
  }

  // TODO(https://fxbug.dev/354043838): Fold into ExpectRootModule/Needed API.
  this->ExpectRootModuleNotLoaded(kFile);
  this->ExpectNeededNotLoaded({kDepFile1, kDepFile2});

  this->Needed({kDepFile2});

  auto res1 = this->DlOpen(kDepFile2, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res1.is_ok()) << res1.error_value();
  EXPECT_TRUE(res1.value());

  auto sym1 = this->DlSym(res1.value(), "foo");
  ASSERT_TRUE(sym1.is_ok()) << sym1.error_value();
  ASSERT_TRUE(sym1.value());

  EXPECT_EQ(RunFunction<int64_t>(sym1.value()), kReturnValueFromFooV2);

  this->ExpectRootModule(kFile);

  this->Needed({kDepFile1});

  auto res2 = this->DlOpen(kFile, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res2.is_ok()) << res2.error_value();
  EXPECT_TRUE(res2.value());

  // Test the `foo` value that is used by the root module's `call_foo()` function.
  auto sym2 = this->DlSym(res2.value(), "call_foo");
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
  auto sym3 = this->DlSym(res2.value(), "foo");
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
  constexpr const char* kFile = "transitive-foo-dep.LocalPrecedenceTransitiveDeps.so";
  constexpr const char* kDepFile1 = "libhas-foo-v1.LocalPrecedenceTransitiveDeps.so";
  constexpr const char* kDepFile2 = "libld-dep-foo-v1.LocalPrecedenceTransitiveDeps.so";
  constexpr const char* kDepFile3 = "libld-dep-foo-v2.LocalPrecedenceTransitiveDeps.so";
  constexpr int64_t kReturnValueFromFooV1 = 2;
  constexpr int64_t kReturnValueFromFooV2 = 7;

  if (!TestFixture::kCanReuseLoadedDeps) {
    GTEST_SKIP() << "test requires that fixture can reuse loaded dependencies";
  }

  // TODO(https://fxbug.dev/354043838): Fold into ExpectRootModule/Needed API.
  this->ExpectRootModuleNotLoaded(kFile);
  this->ExpectNeededNotLoaded({kDepFile1, kDepFile2, kDepFile3});

  this->Needed({kDepFile1, kDepFile2});

  auto res1 = this->DlOpen(kDepFile1, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res1.is_ok()) << res1.error_value();
  EXPECT_TRUE(res1.value());

  auto sym1 = this->DlSym(res1.value(), "call_foo");
  ASSERT_TRUE(sym1.is_ok()) << sym1.error_value();
  ASSERT_TRUE(sym1.value());

  EXPECT_EQ(RunFunction<int64_t>(sym1.value()), kReturnValueFromFooV1);

  this->ExpectRootModule(kFile);
  this->Needed({kDepFile3});

  auto res2 = this->DlOpen(kFile, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res2.is_ok()) << res2.error_value();
  EXPECT_TRUE(res2.value());

  auto sym2 = this->DlSym(res2.value(), "call_foo");
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
  constexpr const char* kFile = "multiple-transitive-foo-deps.LoadedTransitiveDepOrder.so";
  constexpr const char* kDepFile1 = "libhas-foo-v2.LoadedTransitiveDepOrder.so";
  constexpr const char* kDepFile2 = "libld-dep-foo-v2.LoadedTransitiveDepOrder.so";
  constexpr const char* kDepFile3 = "libhas-foo-v1.LoadedTransitiveDepOrder.so";
  constexpr const char* kDepFile4 = "libld-dep-foo-v1.LoadedTransitiveDepOrder.so";
  constexpr int64_t kReturnValueFromFooV1 = 2;
  constexpr int64_t kReturnValueFromFooV2 = 7;

  if constexpr (!TestFixture::kCanReuseLoadedDeps) {
    GTEST_SKIP() << "test requires that fixture can reuse loaded modules for dependencies";
  }

  // TODO(https://fxbug.dev/354043838): Fold into ExpectRootModule/Needed API.
  this->ExpectRootModuleNotLoaded(kFile);
  this->ExpectNeededNotLoaded({kDepFile1, kDepFile2, kDepFile3, kDepFile4});

  this->Needed({kDepFile1, kDepFile2});

  auto res1 = this->DlOpen(kDepFile1, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res1.is_ok()) << res1.error_value();
  EXPECT_TRUE(res1.value());

  auto sym1 = this->DlSym(res1.value(), "call_foo");
  ASSERT_TRUE(sym1.is_ok()) << sym1.error_value();
  ASSERT_TRUE(sym1.value());

  EXPECT_EQ(RunFunction<int64_t>(sym1.value()), kReturnValueFromFooV2);

  this->Needed({kDepFile3, kDepFile4});

  auto res2 = this->DlOpen(kDepFile3, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res2.is_ok()) << res2.error_value();
  EXPECT_TRUE(res2.value());

  auto sym2 = this->DlSym(res2.value(), "call_foo");
  ASSERT_TRUE(sym2.is_ok()) << sym2.error_value();
  ASSERT_TRUE(sym2.value());

  EXPECT_EQ(RunFunction<int64_t>(sym2.value()), kReturnValueFromFooV1);

  this->ExpectRootModule(kFile);

  auto res3 = this->DlOpen(kFile, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res3.is_ok()) << res3.error_value();
  EXPECT_TRUE(res3.value());

  auto sym3 = this->DlSym(res3.value(), "call_foo");
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
  constexpr const char* kFile = "precedence-in-dep-resolution.PrecedenceInDepResolution.so";
  constexpr const char* kDepFile1 = "libbar-v1.PrecedenceInDepResolution.so";
  constexpr const char* kDepFile2 = "libbar-v2.PrecedenceInDepResolution.so";
  constexpr const char* kDepFile3 = "libld-dep-foo-v1.PrecedenceInDepResolution.so";
  constexpr const char* kDepFile4 = "libld-dep-foo-v2.PrecedenceInDepResolution.so";
  constexpr int64_t kReturnValueFromFooV1 = 2;

  // TODO(https://fxbug.dev/354043838): Fold into ExpectRootModule/Needed API.
  this->ExpectRootModuleNotLoaded(kFile);
  this->ExpectNeededNotLoaded({kDepFile1, kDepFile2, kDepFile3, kDepFile4});

  this->ExpectRootModule(kFile);
  this->Needed({kDepFile1, kDepFile2, kDepFile3, kDepFile4});

  auto res1 = this->DlOpen(kFile, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res1.is_ok()) << res1.error_value();
  EXPECT_TRUE(res1.value());

  if constexpr (TestFixture::kDlSymSupportsDeps) {
    auto bar_v1 = this->DlSym(res1.value(), "bar_v1");
    ASSERT_TRUE(bar_v1.is_ok()) << bar_v1.error_value();
    ASSERT_TRUE(bar_v1.value());

    EXPECT_EQ(RunFunction<int64_t>(bar_v1.value()), kReturnValueFromFooV1);

    auto bar_v2 = this->DlSym(res1.value(), "bar_v2");
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
  constexpr const char* kFile =
      "root-precedence-in-dep-resolution.RootPrecedenceInDepResolution.so";
  constexpr const char* kDepFile1 = "libbar-v1.RootPrecedenceInDepResolution.so";
  constexpr const char* kDepFile2 = "libbar-v2.RootPrecedenceInDepResolution.so";
  constexpr const char* kDepFile3 = "libld-dep-foo-v1.RootPrecedenceInDepResolution.so";
  constexpr const char* kDepFile4 = "libld-dep-foo-v2.RootPrecedenceInDepResolution.so";
  constexpr int64_t kReturnValueFromRootModule = 17;
  constexpr int64_t kReturnValueFromFooV1 = 2;
  constexpr int64_t kReturnValueFromFooV2 = 7;

  // TODO(https://fxbug.dev/354043838): Fold into ExpectRootModule/Needed API.
  this->ExpectRootModuleNotLoaded(kFile);
  this->ExpectNeededNotLoaded({kDepFile1, kDepFile2, kDepFile3, kDepFile4});

  this->ExpectRootModule(kFile);
  this->Needed({kDepFile1, kDepFile2, kDepFile3, kDepFile4});

  auto res1 = this->DlOpen(kFile, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res1.is_ok()) << res1.error_value();
  EXPECT_TRUE(res1.value());

  auto foo = this->DlSym(res1.value(), "foo");
  ASSERT_TRUE(foo.is_ok()) << foo.error_value();
  ASSERT_TRUE(foo.value());

  EXPECT_EQ(RunFunction<int64_t>(foo.value()), kReturnValueFromRootModule);

  if constexpr (TestFixture::kDlSymSupportsDeps) {
    auto bar_v1 = this->DlSym(res1.value(), "bar_v1");
    ASSERT_TRUE(bar_v1.is_ok()) << bar_v1.error_value();
    ASSERT_TRUE(bar_v1.value());

    EXPECT_EQ(RunFunction<int64_t>(bar_v1.value()), kReturnValueFromRootModule);

    auto bar_v2 = this->DlSym(res1.value(), "bar_v2");
    ASSERT_TRUE(bar_v2.is_ok()) << bar_v2.error_value();
    ASSERT_TRUE(bar_v2.value());

    EXPECT_EQ(RunFunction<int64_t>(bar_v2.value()), kReturnValueFromRootModule);

    // Test that when we dlopen the dep directly, foo is resolved to the
    // transitive dependency, while bar_v1/bar_v2 continue to use the root
    // module's foo symbol.
    auto dep_file1 = this->DlOpen(kDepFile1, RTLD_NOW | RTLD_LOCAL);
    ASSERT_TRUE(dep_file1.is_ok()) << dep_file1.error_value();
    EXPECT_TRUE(dep_file1.value());

    auto foo1 = this->DlSym(dep_file1.value(), "foo");
    ASSERT_TRUE(foo1.is_ok()) << foo1.error_value();
    ASSERT_TRUE(foo1.value());

    EXPECT_EQ(RunFunction<int64_t>(foo1.value()), kReturnValueFromFooV1);

    auto dep_bar_v1 = this->DlSym(dep_file1.value(), "bar_v1");
    ASSERT_TRUE(dep_bar_v1.is_ok()) << dep_bar_v1.error_value();
    ASSERT_TRUE(dep_bar_v1.value());

    EXPECT_EQ(RunFunction<int64_t>(dep_bar_v1.value()), kReturnValueFromRootModule);

    auto dep_file2 = this->DlOpen(kDepFile2, RTLD_NOW | RTLD_LOCAL);
    ASSERT_TRUE(dep_file2.is_ok()) << dep_file2.error_value();
    EXPECT_TRUE(dep_file2.value());

    auto foo2 = this->DlSym(dep_file2.value(), "foo");
    ASSERT_TRUE(foo2.is_ok()) << foo2.error_value();
    ASSERT_TRUE(foo2.value());

    EXPECT_EQ(RunFunction<int64_t>(foo2.value()), kReturnValueFromFooV2);

    auto dep_bar_v2 = this->DlSym(dep_file2.value(), "bar_v2");
    ASSERT_TRUE(dep_bar_v2.is_ok()) << dep_bar_v2.error_value();
    ASSERT_TRUE(dep_bar_v2.value());

    EXPECT_EQ(RunFunction<int64_t>(dep_bar_v2.value()), kReturnValueFromRootModule);

    ASSERT_TRUE(this->DlClose(dep_file1.value()).is_ok());
    ASSERT_TRUE(this->DlClose(dep_file2.value()).is_ok());
  }

  ASSERT_TRUE(this->DlClose(res1.value()).is_ok());
}

// These are test scenarios that test symbol resolution with RTLD_GLOBAL.

// Test that a previously loaded global module symbol won't affect relative
// relocations in dlopen-ed module.
// dlopen RTLD_GLOBAL foo-v1 -> foo() returns 2
// dlopen relative-reloc-foo -> foo() returns 17
// call foo() from relative-reloc-foo and expect 17.
TYPED_TEST(DlTests, RelativeRelocPrecedence) {
  constexpr const char* kFile1 = "libld-dep-foo-v1.RelativeRelocPrecedence.so";
  constexpr const char* kFile2 = "relative-reloc-foo.RelativeRelocPrecedence.so";
  constexpr int64_t kReturnValueFromGlobal = 2;
  constexpr int64_t kReturnValueFromRelativeReloc = 17;

  if constexpr (!TestFixture::kSupportsGlobalMode) {
    GTEST_SKIP() << "test requires that fixture supports RTLD_GLOBAL";
  }

  // TODO(https://fxbug.dev/354043838): Fold into ExpectRootModule/Needed API.
  this->ExpectNeededNotLoaded({kFile1});
  this->ExpectRootModuleNotLoaded(kFile2);

  this->Needed({kFile1});

  auto res1 = this->DlOpen(kFile1, RTLD_NOW | RTLD_GLOBAL);
  ASSERT_TRUE(res1.is_ok()) << res1.error_value();
  EXPECT_TRUE(res1.value());

  auto sym1 = this->DlSym(res1.value(), "foo");
  ASSERT_TRUE(sym1.is_ok()) << sym1.error_value();
  ASSERT_TRUE(sym1.value());

  EXPECT_EQ(RunFunction<int64_t>(sym1.value()), kReturnValueFromGlobal);

  this->ExpectRootModule(kFile2);

  auto res2 = this->DlOpen(kFile2, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res2.is_ok()) << res2.error_value();
  EXPECT_TRUE(res2.value());

  auto sym2 = this->DlSym(res2.value(), "foo");
  ASSERT_TRUE(sym2.is_ok()) << sym2.error_value();
  ASSERT_TRUE(sym2.value());

  EXPECT_EQ(RunFunction<int64_t>(sym2.value()), kReturnValueFromRelativeReloc);

  ASSERT_TRUE(this->DlClose(res1.value()).is_ok());
  ASSERT_TRUE(this->DlClose(res2.value()).is_ok());
}

// Test that loaded global module will take precedence over dependency ordering.
// dlopen RTLD_GLOBAL foo-v2 -> foo() returns 7
// dlopen has-foo-v1:
//    - foo-v1 -> foo() returns 2
// call foo() from has-foo-v1 and expect foo() to return 7 (from previously
// loaded RTLD_GLOBAL foo-v2).
TYPED_TEST(DlTests, GlobalPrecedence) {
  constexpr const char* kFile1 = "libld-dep-foo-v2.GlobalPrecedence.so";
  constexpr const char* kFile2 = "libhas-foo-v1.GlobalPrecedence.so";
  constexpr const char* kDepFile = "libld-dep-foo-v1.GlobalPrecedence.so";
  constexpr int64_t kReturnValueFromFooV1 = 2;
  constexpr int64_t kReturnValueFromFooV2 = 7;

  if constexpr (!TestFixture::kSupportsGlobalMode) {
    GTEST_SKIP() << "test requires that fixture supports RTLD_GLOBAL";
  }

  // TODO(https://fxbug.dev/354043838): Fold into ExpectRootModule/Needed API.
  this->ExpectNeededNotLoaded({kFile1, kFile2, kDepFile});

  this->Needed({kFile1});

  auto res1 = this->DlOpen(kFile1, RTLD_NOW | RTLD_GLOBAL);
  ASSERT_TRUE(res1.is_ok()) << res1.error_value();
  EXPECT_TRUE(res1.value());

  auto sym1 = this->DlSym(res1.value(), "foo");
  ASSERT_TRUE(sym1.is_ok()) << sym1.error_value();
  ASSERT_TRUE(sym1.value());

  EXPECT_EQ(RunFunction<int64_t>(sym1.value()), kReturnValueFromFooV2);

  this->Needed({kFile2, kDepFile});

  auto res2 = this->DlOpen(kFile2, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res2.is_ok()) << res2.error_value();
  EXPECT_TRUE(res2.value());

  auto sym2 = this->DlSym(res2.value(), "call_foo");
  ASSERT_TRUE(sym2.is_ok()) << sym2.error_value();
  ASSERT_TRUE(sym2.value());

  EXPECT_EQ(RunFunction<int64_t>(sym2.value()), kReturnValueFromFooV2);

  // dlsym will always use dependency ordering from the local scope when looking
  // up a symbol
  auto sym3 = this->DlSym(res2.value(), "foo");
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
  constexpr const char* kFile1 = "libhas-foo-v1.GlobalPrecedenceDeps.so";
  constexpr const char* kDepFile1 = "libld-dep-foo-v1.GlobalPrecedenceDeps.so";
  constexpr const char* kFile2 = "libhas-foo-v2.GlobalPrecedenceDeps.so";
  constexpr const char* kDepFile2 = "libld-dep-foo-v2.GlobalPrecedenceDeps.so";
  constexpr int64_t kReturnValueFromFooV1 = 2;
  constexpr int64_t kReturnValueFromFooV2 = 7;

  if constexpr (!TestFixture::kSupportsGlobalMode) {
    GTEST_SKIP() << "test requires that fixture supports RTLD_GLOBAL";
  }

  // TODO(https://fxbug.dev/354043838): Fold into ExpectRootModule/Needed API.
  this->ExpectNeededNotLoaded({kFile1, kDepFile1, kFile2, kDepFile2});

  this->Needed({kFile1, kDepFile1});

  auto res1 = this->DlOpen(kFile1, RTLD_NOW | RTLD_GLOBAL);
  ASSERT_TRUE(res1.is_ok()) << res1.error_value();
  EXPECT_TRUE(res1.value());

  auto sym1 = this->DlSym(res1.value(), "call_foo");
  ASSERT_TRUE(sym1.is_ok()) << sym1.error_value();
  ASSERT_TRUE(sym1.value());

  EXPECT_EQ(RunFunction<int64_t>(sym1.value()), kReturnValueFromFooV1);

  this->Needed({kFile2, kDepFile2});

  auto res2 = this->DlOpen(kFile2, RTLD_NOW | RTLD_GLOBAL);
  ASSERT_TRUE(res2.is_ok()) << res2.error_value();
  EXPECT_TRUE(res2.value());

  auto sym2 = this->DlSym(res2.value(), "call_foo");
  ASSERT_TRUE(sym2.is_ok()) << sym2.error_value();
  ASSERT_TRUE(sym2.value());

  EXPECT_EQ(RunFunction<int64_t>(sym2.value()), kReturnValueFromFooV1);

  // dlsym will always use dependency ordering from the local scope when looking
  // up a symbol
  auto sym3 = this->DlSym(res2.value(), "foo");
  ASSERT_TRUE(sym3.is_ok()) << sym3.error_value();
  ASSERT_TRUE(sym3.value());

  EXPECT_EQ(RunFunction<int64_t>(sym3.value()), kReturnValueFromFooV2);

  ASSERT_TRUE(this->DlClose(res1.value()).is_ok());
  ASSERT_TRUE(this->DlClose(res2.value()).is_ok());
}

// Test that missing dep will use global symbol if there's a loaded global
// module with the same symbol
// dlopen RTLD global dep-c -> c() returns 2
// dlopen missing-sym -> TestStart() returns 2 + c():
//  - dep-a (does not have c())
// call c() from missing-sym and expect 6 (4 + 2 from previously loaded module).
TYPED_TEST(DlTests, GlobalSatisfiesMissingSymbol) {
  constexpr const char* kFile1 = "libld-dep-c.so";
  constexpr const char* kFile2 = "missing-sym.module.so";
  constexpr const char* kDepFile = "libld-dep-a.so";
  constexpr int64_t kReturnValue = 6;

  if constexpr (!TestFixture::kSupportsGlobalMode) {
    GTEST_SKIP() << "test requires that fixture supports RTLD_GLOBAL";
  }

  // TODO(https://fxbug.dev/354043838): Fold into ExpectRootModule/Needed API.
  this->ExpectNeededNotLoaded({kFile1, kDepFile});
  this->ExpectRootModuleNotLoaded({kFile2});

  this->Needed({kFile1});

  auto res1 = this->DlOpen(kFile1, RTLD_NOW | RTLD_GLOBAL);
  ASSERT_TRUE(res1.is_ok()) << res1.error_value();
  EXPECT_TRUE(res1.value());

  this->ExpectRootModule(kFile2);
  this->Needed({kDepFile});

  auto res2 = this->DlOpen(kFile2, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res2.is_ok()) << res2.error_value();
  EXPECT_TRUE(res2.value());

  auto sym2 = this->DlSym(res2.value(), "TestStart");
  ASSERT_TRUE(sym2.is_ok()) << sym2.error_value();
  ASSERT_TRUE(sym2.value());

  EXPECT_EQ(RunFunction<int64_t>(sym2.value()), kReturnValue);

  // dlsym will not be able to find the global symbol from the local scope
  auto sym3 = this->DlSym(res2.value(), "c");
  ASSERT_TRUE(sym3.is_error()) << sym3.error_value();
  if constexpr (TestFixture::kCanMatchExactError || TestFixture::kEmitsSymbolNotFound) {
    EXPECT_EQ(sym3.error_value().take_str(), "Symbol not found: c");
  } else {
    // emitted by Linux-glibc
    EXPECT_THAT(sym3.error_value().take_str(),
                // only the module name is captured from the full test path
                MatchesRegex("|.*missing-sym.module.so: undefined symbol: c"));
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

}  // namespace
