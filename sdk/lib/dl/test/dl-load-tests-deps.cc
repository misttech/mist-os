// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "dl-load-tests.h"

namespace {

using dl::testing::DlTests;
TYPED_TEST_SUITE(DlTests, dl::testing::TestTypes);

using dl::testing::RunFunction;
using dl::testing::TestModule;
using dl::testing::TestShlib;
using dl::testing::TestSym;

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

}  // namespace
