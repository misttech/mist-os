// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "dl-load-tests.h"

namespace {

using dl::testing::DlTests;
using dl::testing::TestModule;

TYPED_TEST_SUITE(DlTests, dl::testing::TestTypes);

// If RTLD_NOLOAD is passed and the module is not found, NULL is returned but an
// error is not expected.
TYPED_TEST(DlTests, RtldNoLoadBasic) {
  const std::string kRet17File = TestModule("ret17");

  auto open = this->DlOpen(kRet17File.c_str(), RTLD_NOW | RTLD_LOCAL | RTLD_NOLOAD);
  ASSERT_TRUE(open.is_ok()) << open.error_value();
  EXPECT_EQ(open.value(), nullptr);

  // TODO(https://fxbug.dev/325494556): check that dlerror() returns NULL.
}

// TODO(https://fxbug.dev/348727901): test how RTLD_NOLOAD can be used in
// combination with other flags, for example with RTLD_GLOBAL to promote a
// module.

}  // namespace
