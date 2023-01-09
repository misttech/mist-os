// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "util.h"

#include <lib/efi/testing/stub_boot_services.h>

#include "gmock/gmock.h"
#include "xefi.h"

namespace {

const efi_handle kImageHandle = reinterpret_cast<efi_handle>(0x10);

using ::efi::StubBootServices;
using ::testing::_;
using ::testing::ElementsAreArray;
using ::testing::NotNull;
using ::testing::Test;

TEST(Util, Htonll) {
  const uint64_t host_order = 0x0123456789ABCDEFULL;
  const uint64_t net_order = htonll(host_order);

  std::vector<uint8_t> data(sizeof(net_order));
  memcpy(data.data(), &net_order, sizeof(net_order));
  EXPECT_EQ(data, (std::vector<uint8_t>{0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF}));
}

TEST(Util, Ntohll) {
  std::vector<uint8_t> data{0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF};
  uint64_t net_order;
  memcpy(&net_order, data.data(), sizeof(net_order));

  const uint64_t host_order = ntohll(net_order);
  EXPECT_EQ(host_order, 0x0123456789ABCDEFULL);
}

class UtilUefiRealloc : public Test {
 public:
  void SetUp() override {
    // Configure the necessary mocks for memory operation.
    system_table_ = efi_system_table{
        .BootServices = stub_boot_services_.services(),
    };

    xefi_init(kImageHandle, &system_table_);
  }

  void TearDown() override {}

 protected:
  StubBootServices stub_boot_services_;
  // NiceMock<MockBootServices> mock_boot_services_;
  efi_system_table system_table_ = {};
};

// Note that it is necessary to use `malloc()/free()` instead of `new()/delete()`, because stub
// implementation uses `malloc()/free()`;
TEST_F(UtilUefiRealloc, UefiReallocOldSizeIsZero) {
  constexpr size_t kOldSize = 0;
  constexpr size_t kNewSize = 10;
  auto buf = (uint8_t*)malloc(kOldSize);
  auto old_buf = buf;

  EXPECT_TRUE(uefi_realloc((void**)&buf, kOldSize, kNewSize));

  EXPECT_NE(buf, old_buf);
  EXPECT_THAT(buf, NotNull());
  memset(buf, 0, kNewSize);
  free(buf);
}

TEST_F(UtilUefiRealloc, UefiReallocNewSizeIsZero) {
  constexpr size_t kOldSize = 10;
  constexpr size_t kNewSize = 0;
  auto buf = (uint8_t*)malloc(kOldSize);

  EXPECT_TRUE(uefi_realloc((void**)&buf, kOldSize, kNewSize));

  EXPECT_THAT(buf, NotNull());
  // Realloc should handle freeing the memory
}

TEST_F(UtilUefiRealloc, UefiReallocNewSizeSameAsOldSize) {
  constexpr size_t kOldSize = 4;
  constexpr size_t kNewSize = 4;
  constexpr uint8_t kTestVals[] = {0x12, 0x34, 0x56, 0x78};
  auto buf = (uint8_t*)malloc(kOldSize);
  auto old_buf = buf;
  std::copy_n(&kTestVals[0], sizeof(kTestVals), &buf[0]);

  EXPECT_TRUE(uefi_realloc((void**)&buf, kOldSize, kNewSize));

  EXPECT_THAT(buf, NotNull());
  EXPECT_EQ(buf, old_buf);
  EXPECT_THAT(kTestVals, ElementsAreArray(buf, kOldSize));
  free(buf);
}

TEST_F(UtilUefiRealloc, UefiReallocNewSizeLessThanOld) {
  constexpr size_t kOldSize = 4;
  constexpr size_t kNewSize = 3;
  std::vector<uint8_t> kTestVals = {0x12, 0x34, 0x56, 0x78};
  auto buf = (uint8_t*)malloc(kOldSize);
  auto old_buf = buf;
  std::copy_n(kTestVals.begin(), kTestVals.size(), &buf[0]);

  EXPECT_TRUE(uefi_realloc((void**)&buf, kOldSize, kNewSize));

  EXPECT_THAT(buf, NotNull());
  EXPECT_NE(buf, old_buf);
  kTestVals.resize(kNewSize);
  EXPECT_THAT(kTestVals, ElementsAreArray(buf, kNewSize));
  memset(buf, 0, kNewSize);
  free(buf);
}

TEST_F(UtilUefiRealloc, UefiReallocBufIsNull) {
  constexpr size_t kOldSize = 10;
  constexpr size_t kNewSize = 20;
  uint8_t* buf = nullptr;

  EXPECT_TRUE(uefi_realloc((void**)&buf, kOldSize, kNewSize));

  EXPECT_THAT(buf, NotNull());
  memset(buf, 0, kNewSize);
  free(buf);
}

TEST_F(UtilUefiRealloc, UefiReallocContentIsPersistent) {
  constexpr size_t kOldSize = 4;
  constexpr size_t kNewSize = 8;
  constexpr uint8_t kTestVals[] = {0x12, 0x34, 0x56, 0x78};
  auto buf = (uint8_t*)malloc(kOldSize);
  auto old_buf = buf;
  std::copy_n(&kTestVals[0], sizeof(kTestVals), &buf[0]);

  EXPECT_TRUE(uefi_realloc((void**)&buf, kOldSize, kNewSize));

  EXPECT_THAT(buf, NotNull());
  EXPECT_NE(buf, old_buf);
  EXPECT_THAT(kTestVals, ElementsAreArray(buf, kOldSize));
  memset(buf, 0, kNewSize);
  free(buf);
}

TEST(Min, Int) {
  EXPECT_EQ(MIN(0u, 1u), 0u);
  EXPECT_EQ(MIN(-1, 1), -1);
  EXPECT_EQ(MIN(10u, 1u), 1u);
  EXPECT_EQ(MIN(-1, -2), -2);
}

TEST(Max, Int) {
  EXPECT_EQ(MAX(0u, 1u), 1u);
  EXPECT_EQ(MAX(-1, 1), 1);
  EXPECT_EQ(MAX(10u, 1u), 10u);
  EXPECT_EQ(MAX(-1, -2), -1);
}

}  // namespace
