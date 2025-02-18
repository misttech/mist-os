// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "dl-iterate-phdr-tests.h"

#include <gmock/gmock.h>

#include "dl-load-tests.h"

namespace {

using dl::testing::CollectPhdrInfo;
using dl::testing::DlPhdrInfo;
using dl::testing::PhdrInfoList;
using dl::testing::TestModule;
using dl::testing::TestSym;

using dl::testing::DlTests;
TYPED_TEST_SUITE(DlTests, dl::testing::TestTypes);

// Call the system dl_iterate_phdr to collect the phdr info for startup modules
// loaded with this unittest: this serves as the source of truth of what is
// loaded when the test is run.
PhdrInfoList GetStartupPhdrInfo() {
  PhdrInfoList phdr_info;
  dl_iterate_phdr(CollectPhdrInfo, &phdr_info);
  return phdr_info;
}

const PhdrInfoList gStartupPhdrInfo = GetStartupPhdrInfo();

// Test that `dl_iterate_phdr` includes startup modules.
TYPED_TEST(DlTests, DlIteratePhdrStartupModules) {
  PhdrInfoList startup_info_list;
  EXPECT_EQ(this->DlIteratePhdr(CollectPhdrInfo, &startup_info_list), 0);

  // If the dlopen implementation can't unload modules, there will be additional
  // modules that are collected by dl_iterate_phdr that were loaded by tests
  // that ran before this one. If that is the case, we only check that the
  // actual startup modules in `gStartupPhdrInfo` is a subset of the entries
  // collected by this test.
  if (TestFixture::kDlCloseUnloadsModules) {
    EXPECT_EQ(gStartupPhdrInfo, startup_info_list);
  } else {
    EXPECT_THAT(gStartupPhdrInfo, ::testing::IsSubsetOf(startup_info_list));
  }
}

// Test the following as it affects `dl_iterate_phdr` output:
// dlopen module
// `dl_iterate_phdr` output includes new module
// dlclose module
// `dl_iterate_phdr` output doesn't include the module
TYPED_TEST(DlTests, DlIteratePhdrBasic) {
  const std::string kRet17File = TestModule("ret17");

  // Record initial values to compare against during the test.
  PhdrInfoList initial_info_list;
  EXPECT_EQ(this->DlIteratePhdr(CollectPhdrInfo, &initial_info_list), 0);

  DlPhdrInfo last_phdr_info = initial_info_list.back();
  const size_t loaded = last_phdr_info.loaded();
  const size_t unloaded = last_phdr_info.unloaded();

  this->ExpectRootModule(kRet17File);

  auto open_ret17 = this->DlOpen(kRet17File.c_str(), RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(open_ret17.is_ok()) << open_ret17.error_value();
  EXPECT_TRUE(open_ret17.value());

  // Check that a struct `dl_pdhr_info` is produced for the dlopen-ed module.
  PhdrInfoList open_info_list;
  EXPECT_EQ(this->DlIteratePhdr(CollectPhdrInfo, &open_info_list), 0);
  EXPECT_EQ(open_info_list.size(), initial_info_list.size() + 1);

  DlPhdrInfo ret17_phdr_info = open_info_list.back();
  EXPECT_EQ(ret17_phdr_info.name(), kRet17File);

  // Check that the `.dlpi_adds` counter is adjusted.
  EXPECT_EQ(ret17_phdr_info.loaded(), loaded + 1);

  // Look up a symbol from the module and expect that its pointer value should
  // be within the address range of the module's phdrs from its
  // `dl_phdr_info`.
  auto ret17_test_start = this->DlSym(open_ret17.value(), TestSym("TestStart").c_str());
  ASSERT_TRUE(ret17_test_start.is_ok()) << ret17_test_start.error_value();
  EXPECT_TRUE(ret17_test_start.value());

  EXPECT_TRUE(ret17_phdr_info.contains_addr(reinterpret_cast<uintptr_t>(ret17_test_start.value())));

  ASSERT_TRUE(this->DlClose(open_ret17.value()).is_ok());

  // A final check that dl-closing the module will remove its entry and update
  // the `.dlpi_subs` counter.
  PhdrInfoList close_info_list;
  EXPECT_EQ(this->DlIteratePhdr(CollectPhdrInfo, &close_info_list), 0);

  // The last entry should be the same as at the beginning of the test.
  DlPhdrInfo test_last_phdr_info = close_info_list.back();

  if (TestFixture::kDlCloseUnloadsModules) {
    EXPECT_EQ(test_last_phdr_info.unloaded(), unloaded + 1);
    EXPECT_EQ(test_last_phdr_info, last_phdr_info);
    EXPECT_EQ(close_info_list.size(), initial_info_list.size());
  } else {
    // Musl-Fuchsia's dlclose is a no-op and does not change dl_iterate_phdr
    // output: the module entry is preserved.
    EXPECT_EQ(test_last_phdr_info.unloaded(), unloaded);
    EXPECT_EQ(test_last_phdr_info, ret17_phdr_info);
    EXPECT_EQ(close_info_list.size(), open_info_list.size());
  }
}

}  // namespace
