// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <filesystem>

#include <gmock/gmock.h>

#include "dl-load-tests.h"

namespace {

using dl::testing::DlTests;
TYPED_TEST_SUITE(DlTests, dl::testing::TestTypes);

// This is a wrapper around `dl_phdr_info` that makes it easier to
// perform comparisons with other `dl_phdr_info` instances.
struct DlPhdrInfo {
  dl_phdr_info info;

  explicit DlPhdrInfo(dl_phdr_info info) : info(info) {}

  // TODO(https://fxbug.dev/331421403): Check tls_data, tls_modid fields.
  // This will compare module-specific `dl_phdr_info` fields for equality. This
  // does not include equality checks for .dlpi_adds or .dlpi_subs, which are
  // not specific to any one module and are tested outside this operator.
  bool operator==(const DlPhdrInfo& other) const {
    return strcmp(info.dlpi_name, other.info.dlpi_name) == 0 &&
           info.dlpi_addr == other.info.dlpi_addr && info.dlpi_phdr == other.info.dlpi_phdr &&
           info.dlpi_phnum == other.info.dlpi_phnum;
  }
};

using PhdrInfoList = std::vector<DlPhdrInfo>;

// A `dl_iterate_phdr` callback that collects the dl_iterate_phdr information of
// all loaded modules. This will push an instance of the `DlPhdrInfo` to the
// `PhdrInfo*` that is passed in via the `data` pointer.
int CollectPhdrInfo(dl_phdr_info* phdr_info, size_t size, void* data) {
  static_cast<PhdrInfoList*>(data)->emplace_back(*phdr_info);
  return 0;
}

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
  if constexpr (!TestFixture::kProvidesDlIteratePhdr) {
    GTEST_SKIP() << "test requires dl_iterate_phdr";
  }

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

}  // namespace
