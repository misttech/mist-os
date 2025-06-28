// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "load-tests.h"

namespace ld::testing {
namespace {

TYPED_TEST(LdLoadTests, Relative) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Load("relative-reloc"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, Symbolic) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Load("symbolic-reloc"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, SymbolicNamespace) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Needed({"libld-dep-a.so"}));

  ASSERT_NO_FATAL_FAILURE(this->Load("symbolic-namespace"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, OneWeakSymbol) {
  constexpr int64_t kFooV2ReturnValue = 7;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Needed({"libld-dep-foo-v1-weak.so", "libld-dep-foo-v2.so"}));

  ASSERT_NO_FATAL_FAILURE(this->Load("one-weak-symbol"));

  // TODO(https://fxbug.dev/428046261): libld will resolve to the strong
  // definition found in libld-dep-foo-v2. Eventually, when strict link order
  // resolution is enforced, libld will use the first definition found in the
  // dependency set (from libld-dep-foo-v1).
  EXPECT_EQ(this->Run(), kFooV2ReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, AllWeakSymbols) {
  constexpr int64_t kFooV1ReturnValue = 2;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Needed({"libld-dep-foo-v1-weak.so", "libld-dep-foo-v2-weak.so"}));

  ASSERT_NO_FATAL_FAILURE(this->Load("all-weak-symbols"));

  EXPECT_EQ(this->Run(), kFooV1ReturnValue);

  this->ExpectLog("");
}

}  // namespace
}  // namespace ld::testing
