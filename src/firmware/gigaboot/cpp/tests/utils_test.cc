// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include "utils.h"

#include <algorithm>

#include <efi/global-variable.h>
#include <efi/types.h>
#include <gtest/gtest.h>

#include "gpt.h"
#include "mock_boot_service.h"

namespace gigaboot {
namespace {

TEST(GigabootTest, PrintTpm2Capability) {
  MockStubService stub_service;
  Device image_device({"path", "image"});  // dont care
  Tcg2Device tcg2_device;
  auto cleanup = SetupEfiGlobalState(stub_service, image_device);

  stub_service.AddDevice(&image_device);
  stub_service.AddDevice(&tcg2_device);

  ASSERT_EQ(PrintTpm2Capability(), EFI_SUCCESS);
}

TEST(GigabootTest, PrintTpm2CapabilityTpm2NotSupported) {
  MockStubService stub_service;
  Device image_device({"path", "image"});  // dont care
  auto cleanup = SetupEfiGlobalState(stub_service, image_device);
  stub_service.AddDevice(&image_device);
  ASSERT_NE(PrintTpm2Capability(), EFI_SUCCESS);
}

uint8_t secureboot_val = 0;
EFIAPI efi_status test_get_secureboot_var(char16_t* name, efi_guid* guid, uint32_t* flags,
                                          size_t* length, void* data) {
  const char16_t kVariable[] = u"SecureBoot";
  EXPECT_EQ(0, memcmp(kVariable, name, sizeof(kVariable)));
  EXPECT_EQ(0, memcmp(&GlobalVariableGuid, guid, sizeof(GlobalVariableGuid)));
  EXPECT_EQ(*length, 1ULL);
  memcpy(data, &secureboot_val, 1);
  return EFI_SUCCESS;
}

TEST(GigabootTest, IsSecureBootOn) {
  MockStubService stub_service;
  Device image_device({"path", "image"});  // dont care
  auto cleanup = SetupEfiGlobalState(stub_service, image_device);
  efi_runtime_services services{
      .GetVariable = test_get_secureboot_var,
  };
  gEfiSystemTable->RuntimeServices = &services;

  secureboot_val = 0;
  auto ret = IsSecureBootOn();
  ASSERT_TRUE(ret.is_ok());
  EXPECT_FALSE(*ret);

  secureboot_val = 1;
  ret = IsSecureBootOn();
  ASSERT_TRUE(ret.is_ok());
  EXPECT_TRUE(*ret);
}

EFIAPI efi_status test_get_secureboot_fail(char16_t*, efi_guid*, uint32_t*, size_t*, void*) {
  return EFI_NOT_FOUND;
}

TEST(GigabootTest, IsSecureBootOnReturnsFalseOnError) {
  MockStubService stub_service;
  Device image_device({"path", "image"});  // dont care
  auto cleanup = SetupEfiGlobalState(stub_service, image_device);
  efi_runtime_services services{
      .GetVariable = test_get_secureboot_fail,
  };
  gEfiSystemTable->RuntimeServices = &services;
  auto ret = IsSecureBootOn();
  ASSERT_TRUE(ret.is_error());
}

struct DynamicPartitionNameTestCase {
  const char* partition_name;
  const char* lookup_name;
};

using DynamicPartitionNameTest = ::testing::TestWithParam<DynamicPartitionNameTestCase>;

TEST_P(DynamicPartitionNameTest, TestDynamicPartitionMapping) {
  MockStubService stub_service;
  Device image_device({"path-A", "path-B", "path-C", "image"});
  BlockDevice block_device({"path-A", "path-B", "path-C"}, 1024);
  auto cleanup = SetupEfiGlobalState(stub_service, image_device);

  stub_service.AddDevice(&image_device);
  stub_service.AddDevice(&block_device);

  block_device.InitializeGpt();

  const DynamicPartitionNameTestCase& test_case = GetParam();
  gpt_entry_t entry{{}, {}, kGptFirstUsableBlocks, kGptFirstUsableBlocks + 5, 0, {}};
  SetGptEntryName(test_case.partition_name, entry);
  block_device.AddGptPartition(entry);

  auto res = FindEfiGptDevice();
  ASSERT_TRUE(res.is_ok());

  EfiGptBlockDevice gpt_device = std::move(res.value());
  ASSERT_TRUE(gpt_device.Load().is_ok());

  std::string_view mapped_name = MaybeMapPartitionName(gpt_device, test_case.lookup_name);
  ASSERT_EQ(mapped_name, test_case.partition_name);
}

INSTANTIATE_TEST_SUITE_P(
    DynamicPartitionNamesTests, DynamicPartitionNameTest,
    testing::ValuesIn<DynamicPartitionNameTest::ParamType>({
        {GPT_ZIRCON_A_NAME, GPT_ZIRCON_A_NAME},
        {GPT_ZIRCON_B_NAME, GPT_ZIRCON_B_NAME},
        {GPT_ZIRCON_R_NAME, GPT_ZIRCON_R_NAME},
        {GPT_DURABLE_BOOT_NAME, GPT_DURABLE_BOOT_NAME},
        {GUID_ZIRCON_A_NAME, GPT_ZIRCON_A_NAME},
        {GUID_ZIRCON_B_NAME, GPT_ZIRCON_B_NAME},
        {GUID_ZIRCON_R_NAME, GPT_ZIRCON_R_NAME},
        {GUID_ABR_META_NAME, GPT_DURABLE_BOOT_NAME},
    }),
    [](testing::TestParamInfo<DynamicPartitionNameTest::ParamType> const& info) {
      // Only alphanumeric chars and _ are allowed in gtest names.
      std::string name(info.param.lookup_name);
      std::replace(name.begin(), name.end(), '-', '_');
      return name + "_" + std::string(name == info.param.partition_name ? "modern" : "legacy");
    });

}  // namespace
}  // namespace gigaboot
