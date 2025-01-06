// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "dl-load-tests.h"
#include "startup-symbols.h"

namespace {

using dl::testing::DlTests;
TYPED_TEST_SUITE(DlTests, dl::testing::TestTypes);

using dl::testing::IsUndefinedSymbolErrMsg;
using dl::testing::RunFunction;
using dl::testing::TestModule;
using dl::testing::TestShlib;
using dl::testing::TestSym;

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

}  // namespace
