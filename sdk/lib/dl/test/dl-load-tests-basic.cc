// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "dl-load-tests.h"

namespace {

using dl::testing::DlTests;
TYPED_TEST_SUITE(DlTests, dl::testing::TestTypes);

using dl::testing::RunFunction;
using dl::testing::TestModule;
using dl::testing::TestSym;

// Load a basic file with no dependencies.
TYPED_TEST(DlTests, Basic) {
  constexpr int64_t kReturnValue = 17;
  const std::string kRet17File = TestModule("ret17");

  this->ExpectRootModule(kRet17File);

  auto open = this->DlOpen(kRet17File.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(open.is_ok()) << open.error_value();
  EXPECT_TRUE(open.value());

  // Look up the TestSym("TestStart").c_str() function and call it, expecting it to return 17.
  auto sym = this->DlSym(open.value(), TestSym("TestStart").c_str());
  ASSERT_TRUE(sym.is_ok()) << sym.error_value();
  ASSERT_TRUE(sym.value());

  EXPECT_EQ(RunFunction<int64_t>(sym.value()), kReturnValue);

  ASSERT_TRUE(this->DlClose(open.value()).is_ok());
}

// Load a file that performs relative relocations against itself. The TestStart
// function's return value is derived from the resolved symbols.
TYPED_TEST(DlTests, Relative) {
  constexpr int64_t kReturnValue = 17;
  const std::string kRelativeRelocFile = TestModule("relative-reloc");

  this->ExpectRootModule(kRelativeRelocFile);

  auto open = this->DlOpen(kRelativeRelocFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(open.is_ok()) << open.error_value();
  EXPECT_TRUE(open.value());

  auto sym = this->DlSym(open.value(), TestSym("TestStart").c_str());
  ASSERT_TRUE(sym.is_ok()) << sym.error_value();
  ASSERT_TRUE(sym.value());

  EXPECT_EQ(RunFunction<int64_t>(sym.value()), kReturnValue);

  ASSERT_TRUE(this->DlClose(open.value()).is_ok());
}

// Load a file that performs symbolic relocations against itself. The TestStart
// functions' return value is derived from the resolved symbols.
TYPED_TEST(DlTests, Symbolic) {
  constexpr int64_t kReturnValue = 17;
  const std::string kSymbolicFile = TestModule("symbolic-reloc");

  this->ExpectRootModule(kSymbolicFile);

  auto open = this->DlOpen(kSymbolicFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(open.is_ok()) << open.error_value();
  EXPECT_TRUE(open.value());

  auto sym = this->DlSym(open.value(), TestSym("TestStart").c_str());
  ASSERT_TRUE(sym.is_ok()) << sym.error_value();
  ASSERT_TRUE(sym.value());

  EXPECT_EQ(RunFunction<int64_t>(sym.value()), kReturnValue);

  ASSERT_TRUE(this->DlClose(open.value()).is_ok());
}

// Test loading a module with relro protections.
TYPED_TEST(DlTests, Relro) {
  const std::string kRelroFile = TestModule("relro");

  this->ExpectRootModule(kRelroFile);

  auto open = this->DlOpen(kRelroFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(open.is_ok()) << open.error_value();
  ASSERT_TRUE(open.value());

  auto sym = this->DlSym(open.value(), TestSym("TestStart").c_str());
  ASSERT_TRUE(sym.is_ok()) << sym.error_value();
  ASSERT_TRUE(sym.value());

  EXPECT_DEATH(RunFunction<int64_t>(sym.value()), "");

  ASSERT_TRUE(this->DlClose(open.value()).is_ok());
}

// Test that calling dlopen twice on a file will return the same pointer,
// indicating that the dynamic linker is storing the module in its bookkeeping.
// dlsym() should return a pointer to the same symbol from the same module as
// well.
TYPED_TEST(DlTests, BasicModuleReuse) {
  const std::string kRet17File = TestModule("ret17");

  this->ExpectRootModule(kRet17File);

  auto first_open_ret17 = this->DlOpen(kRet17File.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(first_open_ret17.is_ok()) << first_open_ret17.error_value();
  EXPECT_TRUE(first_open_ret17.value());

  auto second_open_ret17 = this->DlOpen(kRet17File.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(second_open_ret17.is_ok()) << second_open_ret17.error_value();
  EXPECT_TRUE(second_open_ret17.value());

  EXPECT_EQ(first_open_ret17.value(), second_open_ret17.value());

  auto first_ret17_test_start = this->DlSym(first_open_ret17.value(), TestSym("TestStart").c_str());
  ASSERT_TRUE(first_ret17_test_start.is_ok()) << first_ret17_test_start.error_value();
  EXPECT_TRUE(first_ret17_test_start.value());

  auto second_ret17_test_start =
      this->DlSym(second_open_ret17.value(), TestSym("TestStart").c_str());
  ASSERT_TRUE(second_ret17_test_start.is_ok()) << second_ret17_test_start.error_value();
  EXPECT_TRUE(second_ret17_test_start.value());

  EXPECT_EQ(first_ret17_test_start.value(), second_ret17_test_start.value());

  ASSERT_TRUE(this->DlClose(first_open_ret17.value()).is_ok());
  ASSERT_TRUE(this->DlClose(second_open_ret17.value()).is_ok());
}

// Test that different mutually-exclusive kFiles that were dlopen-ed do not share
// pointers or resolved symbols.
TYPED_TEST(DlTests, UniqueModules) {
  const std::string kRet17File = TestModule("ret17");
  const std::string kRet23File = TestModule("ret23");

  this->ExpectRootModule(kRet17File);

  auto open_ret17 = this->DlOpen(kRet17File.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(open_ret17.is_ok()) << open_ret17.error_value();
  EXPECT_TRUE(open_ret17.value());

  this->ExpectRootModule(kRet23File);

  auto open_ret23 = this->DlOpen(kRet23File.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(open_ret23.is_ok()) << open_ret23.error_value();
  EXPECT_TRUE(open_ret23.value());

  EXPECT_NE(open_ret17.value(), open_ret23.value());

  auto ret17_test_start = this->DlSym(open_ret17.value(), TestSym("TestStart").c_str());
  ASSERT_TRUE(ret17_test_start.is_ok()) << ret17_test_start.error_value();
  EXPECT_TRUE(ret17_test_start.value());

  auto sym23_test_start = this->DlSym(open_ret23.value(), TestSym("TestStart").c_str());
  ASSERT_TRUE(sym23_test_start.is_ok()) << sym23_test_start.error_value();
  EXPECT_TRUE(sym23_test_start.value());

  EXPECT_NE(ret17_test_start.value(), sym23_test_start.value());

  EXPECT_EQ(RunFunction<int64_t>(ret17_test_start.value()), 17);
  EXPECT_EQ(RunFunction<int64_t>(sym23_test_start.value()), 23);

  ASSERT_TRUE(this->DlClose(open_ret17.value()).is_ok());
  ASSERT_TRUE(this->DlClose(open_ret23.value()).is_ok());
}

}  // namespace
