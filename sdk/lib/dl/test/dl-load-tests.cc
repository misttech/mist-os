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
  constexpr const char* kNotFoundFile = "does-not-exist.so";

  this->ExpectMissing(kNotFoundFile);

  auto result = this->DlOpen(kNotFoundFile, RTLD_NOW | RTLD_LOCAL);
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
  constexpr const char* kBasicFile = "ret17.module.so";

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

  auto result = this->DlOpen(kBasicFile, bad_mode);
  ASSERT_TRUE(result.is_error());
  EXPECT_EQ(result.error_value().take_str(), "invalid mode parameter")
      << "for mode argument " << bad_mode;
}

// Load a basic file with no dependencies.
TYPED_TEST(DlTests, Basic) {
  constexpr int64_t kReturnValue = 17;
  constexpr const char* kBasicFile = "ret17.module.so";

  this->ExpectRootModule(kBasicFile);

  auto result = this->DlOpen(kBasicFile, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_ok()) << result.error_value();
  EXPECT_TRUE(result.value());

  // Look up the "TestStart" function and call it, expecting it to return 17.
  auto sym_result = this->DlSym(result.value(), "TestStart");
  ASSERT_TRUE(sym_result.is_ok()) << result.error_value();
  ASSERT_TRUE(sym_result.value());

  EXPECT_EQ(RunFunction<int64_t>(sym_result.value()), kReturnValue);
}

// Load a file that performs relative relocations against itself. The TestStart
// function's return value is derived from the resolved symbols.
TYPED_TEST(DlTests, Relative) {
  constexpr int64_t kReturnValue = 17;
  constexpr const char* kRelativeRelocFile = "relative-reloc.module.so";

  this->ExpectRootModule(kRelativeRelocFile);

  auto result = this->DlOpen(kRelativeRelocFile, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_ok()) << result.error_value();
  EXPECT_TRUE(result.value());

  auto sym_result = this->DlSym(result.value(), "TestStart");
  ASSERT_TRUE(sym_result.is_ok()) << result.error_value();
  ASSERT_TRUE(sym_result.value());

  EXPECT_EQ(RunFunction<int64_t>(sym_result.value()), kReturnValue);
}

// Load a file that performs symbolic relocations against itself. The TestStart
// functions' return value is derived from the resolved symbols.
TYPED_TEST(DlTests, Symbolic) {
  constexpr int64_t kReturnValue = 17;
  constexpr const char* kSymbolicRelocFile = "symbolic-reloc.module.so";

  this->ExpectRootModule(kSymbolicRelocFile);

  auto result = this->DlOpen(kSymbolicRelocFile, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_ok()) << result.error_value();
  EXPECT_TRUE(result.value());

  auto sym_result = this->DlSym(result.value(), "TestStart");
  ASSERT_TRUE(sym_result.is_ok()) << result.error_value();
  ASSERT_TRUE(sym_result.value());

  EXPECT_EQ(RunFunction<int64_t>(sym_result.value()), kReturnValue);
}

// Load a module that depends on a symbol provided directly by a dependency.
TYPED_TEST(DlTests, BasicDep) {
  constexpr int64_t kReturnValue = 17;

  constexpr const char* kBasicDepFile = "basic-dep.module.so";

  this->ExpectRootModule(kBasicDepFile);
  this->Needed({"libld-dep-a.so"});

  auto result = this->DlOpen(kBasicDepFile, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_ok()) << result.error_value();
  EXPECT_TRUE(result.value());

  auto sym_result = this->DlSym(result.value(), "TestStart");
  ASSERT_TRUE(sym_result.is_ok()) << result.error_value();
  ASSERT_TRUE(sym_result.value());

  EXPECT_EQ(RunFunction<int64_t>(sym_result.value()), kReturnValue);
}

// Load a module that depends on a symbols provided directly and transitively by
// several dependencies. Dependency ordering is serialized such that a module
// depends on a symbol provided by a dependency only one hop away
// (e.g. in its DT_NEEDED list):
TYPED_TEST(DlTests, IndirectDeps) {
  constexpr int64_t kReturnValue = 17;

  constexpr const char* kIndirectDepsFile = "indirect-deps.module.so";

  this->ExpectRootModule(kIndirectDepsFile);
  this->Needed({"libindirect-deps-a.so", "libindirect-deps-b.so", "libindirect-deps-c.so"});

  auto result = this->DlOpen(kIndirectDepsFile, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_ok()) << result.error_value();
  EXPECT_TRUE(result.value());

  auto sym_result = this->DlSym(result.value(), "TestStart");
  ASSERT_TRUE(sym_result.is_ok()) << result.error_value();
  ASSERT_TRUE(sym_result.value());

  EXPECT_EQ(RunFunction<int64_t>(sym_result.value()), kReturnValue);
}

// Load a module that depends on symbols provided directly and transitively by
// several dependencies. Dependency ordering is DAG-like where several modules
// share a dependency.
TYPED_TEST(DlTests, ManyDeps) {
  constexpr int64_t kReturnValue = 17;

  constexpr const char* kManyDepsFile = "many-deps.module.so";

  this->ExpectRootModule(kManyDepsFile);
  this->Needed({
      "libld-dep-a.so",
      "libld-dep-b.so",
      "libld-dep-f.so",
      "libld-dep-c.so",
      "libld-dep-d.so",
      "libld-dep-e.so",
  });

  auto result = this->DlOpen(kManyDepsFile, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_ok()) << result.error_value();
  EXPECT_TRUE(result.value());

  auto sym_result = this->DlSym(result.value(), "TestStart");
  ASSERT_TRUE(sym_result.is_ok()) << result.error_value();
  ASSERT_TRUE(sym_result.value());

  EXPECT_EQ(RunFunction<int64_t>(sym_result.value()), kReturnValue);
}

// TODO(https://fxbug.dev/339028040): Test missing symbol in transitive dep.
// Load a module that depends on libld-dep-a.so, but this dependency does not
// provide the b symbol referenced by the root module, so relocation fails.
TYPED_TEST(DlTests, MissingSymbol) {
  constexpr const char* kMissingSymbolFile = "missing-sym.module.so";

  this->ExpectRootModule(kMissingSymbolFile);
  this->Needed({"libld-dep-a.so"});

  auto result = this->DlOpen(kMissingSymbolFile, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_error());
  if constexpr (TestFixture::kCanMatchExactError) {
    EXPECT_EQ(result.error_value().take_str(), "missing-sym.module.so: undefined symbol: b");
  } else {
    EXPECT_THAT(result.error_value().take_str(),
                MatchesRegex(
                    // emitted by Fuchsia-musl
                    "Error relocating missing-sym.module.so: b: symbol not found"
                    // emitted by Linux-glibc
                    "|.*missing-sym.module.so: undefined symbol: b"));
  }
}

// Try to load a module that has a (direct) dependency that cannot be found.
TYPED_TEST(DlTests, MissingDependency) {
  constexpr const char* kMissingDepFile = "missing-dep.module.so";

  this->ExpectRootModule(kMissingDepFile);
  this->Needed({NotFound("libmissing-dep-dep.so")});

  auto result = this->DlOpen(kMissingDepFile, RTLD_NOW | RTLD_LOCAL);
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
  constexpr const char* kMissingDepFile = "missing-transitive-dep.module.so";

  this->ExpectRootModule(kMissingDepFile);
  this->Needed({Found("libhas-missing-dep.so"), NotFound("libmissing-dep-dep.so")});

  auto result = this->DlOpen(kMissingDepFile, RTLD_NOW | RTLD_LOCAL);
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

// Test that calling dlopen twice on a file will return the same pointer,
// indicating that the dynamic linker is storing the module in its bookkeeping.
// dlsym() should return a pointer to the same symbol from the same module as
// well.
TYPED_TEST(DlTests, BasicModuleReuse) {
  constexpr const char* kBasicFile = "ret17.module.so";

  this->ExpectRootModule(kBasicFile);

  auto res1 = this->DlOpen(kBasicFile, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(res1.is_ok()) << res1.error_value();
  auto ptr1 = res1.value();
  EXPECT_TRUE(ptr1);

  auto res2 = this->DlOpen(kBasicFile, RTLD_NOW | RTLD_LOCAL);
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
}

// Test that different mutually-exclusive files that were dlopen-ed do not share
// pointers or resolved symbols.
TYPED_TEST(DlTests, UniqueModules) {
  constexpr const char* kRet17 = "ret17.module.so";
  constexpr int64_t kReturnValue17 = 17;
  constexpr const char* kRet23 = "ret23.module.so";
  constexpr int64_t kReturnValue23 = 23;

  this->ExpectRootModule(kRet17);

  auto ret17 = this->DlOpen(kRet17, RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(ret17.is_ok()) << ret17.error_value();
  auto ret17_ptr = ret17.value();
  EXPECT_TRUE(ret17_ptr);

  this->ExpectRootModule(kRet23);

  auto ret23 = this->DlOpen(kRet23, RTLD_NOW | RTLD_LOCAL);
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

  EXPECT_EQ(RunFunction<int64_t>(sym17_ptr), kReturnValue17);
  EXPECT_EQ(RunFunction<int64_t>(sym23_ptr), kReturnValue23);
}

}  // namespace
