// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "dl-load-tests.h"
#include "startup-symbols.h"

namespace {

using dl::testing::DlTests;
TYPED_TEST_SUITE(DlTests, dl::testing::TestTypes);

// dlopen a module whose initializers and finalizers are decoded by legacy
// DT_INIT and DT_FINI sections. These functions will update the global
// gInitFiniState, and that value is checked in this test to ensure those
// functions were run.
TYPED_TEST(DlTests, InitFiniLegacy) {
  const std::string kFile = "init-fini-legacy.so";

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
  const std::string kFile = "init-fini-array.so";

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

// Test that dlopen will run initializers and finalizers of a module with
// dependencies that also have initializers and finalizers. Each init/fini
// function expects the gInitFiniState to be set to a particular value before
// it's updated.
//
// dlopen init-fini-array-root:
//   - init-fini-array-a:
//     - init-fini-array-a-dep
//   - init-fini-array-b:
//     - init-fini-array-b-dep
//   - init-fini-array-c
//
// Module initializers are run in this order:
//   init-fini-array-b-dep
//   init-fini-array-a-dep
//   init-fini-array-c
//   init-fini-array-b
//   init-fini-array-a
//   init-fini-array-root
//
// Module finalizers are run in reverse of the init order:
//   init-fini-array-root
//   init-fini-array-a
//   init-fini-array-b
//   init-fini-array-c
//   init-fini-array-a-dep
//   init-fini-array-b-dep
TYPED_TEST(DlTests, InitFiniArrayWithDeps) {
  const std::string kFile = "init-fini-array-with-deps.so";
  const std::string kAFile = "libinit-fini-array-a.so";
  const std::string kADepFile = "libinit-fini-array-a-dep.so";
  const std::string kBFile = "libinit-fini-array-b.so";
  const std::string kBDepFile = "libinit-fini-array-b-dep.so";
  const std::string kCFile = "libinit-fini-array-c.so";

  ASSERT_EQ(gInitFiniState, 0);

  this->ExpectRootModule(kFile);
  this->Needed({kAFile, kBFile, kCFile, kADepFile, kBDepFile});

  auto open = this->DlOpen(kFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(open.is_ok()) << open.error_value();
  EXPECT_TRUE(open.value()) << open.error_value();

  EXPECT_EQ(gInitFiniState, 6);

  ASSERT_TRUE(this->DlClose(open.value()).is_ok());

  if (TestFixture::kDlCloseCanRunFinalizers) {
    EXPECT_EQ(gInitFiniState, 12);
  }

  gInitFiniState = 0;
}

// dlopen a module with a mix of DT_INIT/DT_FINI and DT_INIT_ARRAY and
// DT_FINI_ARRAY entries.
TYPED_TEST(DlTests, InitFiniArrayWithLegacy) {
  const std::string kFile = "init-fini-array-with-legacy.so";

  ASSERT_EQ(gInitFiniState, 0);

  this->ExpectRootModule(kFile);

  auto open = this->DlOpen(kFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(open.is_ok()) << open.error_value();
  EXPECT_TRUE(open.value()) << open.error_value();

  EXPECT_EQ(gInitFiniState, 202);

  ASSERT_TRUE(this->DlClose(open.value()).is_ok());

  if (TestFixture::kDlCloseCanRunFinalizers) {
    EXPECT_EQ(gInitFiniState, 204);
  }

  gInitFiniState = 0;
}

}  // namespace
