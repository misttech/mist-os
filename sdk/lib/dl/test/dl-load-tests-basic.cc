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

}  // namespace
