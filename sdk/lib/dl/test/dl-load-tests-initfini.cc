// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "dl-load-tests.h"
#include "startup-symbols.h"

namespace {

using dl::testing::DlTests;
TYPED_TEST_SUITE(DlTests, dl::testing::TestTypes);

// This is used to check that module initializers and finalizers are run in a
// specific order. Init and fini functions call RegisterInit and RegisterFini to
// add a value to a collection that gets checked by CheckInit and CheckFini in
// tests. This class is derived from the RegisterInitFini class defined in
// //sdk/lib/ld/test/modules/startup-symbols.h.
class InitFiniCheck : public RegisterInitFini {
 public:
  virtual ~InitFiniCheck() {
    // Expect the test to have validated all registered init/fini values before
    // it ends.
    EXPECT_TRUE(actual_init_.empty());
    EXPECT_TRUE(actual_fini_.empty());

    // Make sure tests reset this pointer to NULL. If a module didn't get
    // unloaded (either because dlcose didn't unload it or because dlclose was
    // never called), its finalizers will still be run at exit; setting this
    // global back to NULL will prevent those finalizers from accessing the
    // GC-ed instance.
    EXPECT_FALSE(gRegisterInitFini);
  }

  // Expect that the init values that have been registered so far are in the
  // given expected order.
  void CheckInit(std::vector<int> expected) {
    EXPECT_EQ(expected, actual_init_);
    // Clear state for the next CheckInit call.
    actual_init_.clear();
  }

  // Similar to CheckFini, check that fini values are registered in the
  // expected order.
  void CheckFini(std::vector<int> expected) {
    EXPECT_EQ(expected, actual_fini_);
    actual_fini_.clear();
  }

  // These functions are called by test module code to register a specific value
  // that should be checked by the test.
  void RegisterInit(int id) override { actual_init_.push_back(id); }

  void RegisterFini(int id) override { actual_fini_.push_back(id); }

 private:
  std::vector<int> actual_init_;
  std::vector<int> actual_fini_;
};

// dlopen a module whose initializers and finalizers are decoded by legacy
// DT_INIT and DT_FINI sections. These functions will call a callback with a
// value that is checked by the test to ensure those functions were run in order.
TYPED_TEST(DlTests, InitFiniLegacy) {
  const std::string kFile = "init-fini-legacy.so";

  InitFiniCheck init_fini_check{};
  gRegisterInitFini = &init_fini_check;

  this->ExpectRootModule(kFile);

  auto open = this->DlOpen(kFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(open.is_ok()) << open.error_value();
  EXPECT_TRUE(open.value()) << open.error_value();

  init_fini_check.CheckInit({101});

  ASSERT_TRUE(this->DlClose(open.value()).is_ok());

  if (TestFixture::kDlCloseCanRunFinalizers) {
    init_fini_check.CheckFini({102});
  }

  gRegisterInitFini = nullptr;
}

// Similar to InitFiniLegacy test, except dlopen a module whose initializers and
// finalizers are decoded from DT_INIT_ARRAY/DT_FINI_ARRAY sections. This also
// tests that multiple initializers/finalizers in the dlopen-ed module are run in
// correct order.
TYPED_TEST(DlTests, InitFiniArray) {
  const std::string kFile = "init-fini-array.so";

  InitFiniCheck init_fini_check;
  gRegisterInitFini = &init_fini_check;

  this->ExpectRootModule(kFile);

  auto open = this->DlOpen(kFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(open.is_ok()) << open.error_value();
  EXPECT_TRUE(open.value()) << open.error_value();

  // Expect the three ctors to have run in the order expected by the functions
  // in init-fini-array.cc
  init_fini_check.CheckInit({0, 1, 2});

  ASSERT_TRUE(this->DlClose(open.value()).is_ok());

  // Expect the three dtors to have run in the order expected by the functions
  // in init-fini-array.cc
  if (TestFixture::kDlCloseCanRunFinalizers) {
    init_fini_check.CheckFini({3, 4, 5});
  }

  gRegisterInitFini = nullptr;
}

// Test that dlopen will run initializers and finalizers of a module with
// dependencies that also have initializers and finalizers. Similar to the above
// tests, each init/fini function calls a callback with a particular value that
// gets checked by the test.
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

  InitFiniCheck init_fini_check;
  gRegisterInitFini = &init_fini_check;

  this->ExpectRootModule(kFile);
  this->Needed({kAFile, kBFile, kCFile, kADepFile, kBDepFile});

  auto open = this->DlOpen(kFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(open.is_ok()) << open.error_value();
  EXPECT_TRUE(open.value()) << open.error_value();

  init_fini_check.CheckInit({0, 1, 2, 3, 4, 5});

  ASSERT_TRUE(this->DlClose(open.value()).is_ok());

  if (TestFixture::kDlCloseCanRunFinalizers) {
    init_fini_check.CheckFini({6, 7, 8, 9, 10, 11});
  }

  gRegisterInitFini = nullptr;
}

// dlopen a module with a mix of DT_INIT/DT_FINI and DT_INIT_ARRAY and
// DT_FINI_ARRAY entries.
TYPED_TEST(DlTests, InitFiniArrayWithLegacy) {
  const std::string kFile = "init-fini-array-with-legacy.so";

  InitFiniCheck init_fini_check;
  gRegisterInitFini = &init_fini_check;

  this->ExpectRootModule(kFile);

  auto open = this->DlOpen(kFile.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(open.is_ok()) << open.error_value();
  EXPECT_TRUE(open.value()) << open.error_value();

  init_fini_check.CheckInit({201, 202});

  ASSERT_TRUE(this->DlClose(open.value()).is_ok());

  if (TestFixture::kDlCloseCanRunFinalizers) {
    init_fini_check.CheckFini({203, 204});
  }

  gRegisterInitFini = nullptr;
}

}  // namespace
