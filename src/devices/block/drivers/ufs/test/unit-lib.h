// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_UFS_TEST_UNIT_LIB_H_
#define SRC_DEVICES_BLOCK_DRIVERS_UFS_TEST_UNIT_LIB_H_

#include <zxtest/zxtest.h>

#include "mock-device/ufs-mock-device.h"
#include "src/devices/block/drivers/ufs/ufs.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace ufs {

class UfsTest : public zxtest::Test {
 public:
  // To access the Data struct declared as protect, we re-declare it as public.
  using ResponseUpiuData = ResponseUpiu::Data;
  using CommandUpiuData = CommandUpiu::Data;
  using QueryResponseUpiuData = QueryResponseUpiu::Data;
  using QueryRequestUpiuData = QueryRequestUpiu::Data;
  using NopInUpiuData = NopInUpiu::Data;

  void SetUp() override;

  void RunInit();

  void TearDown() override;

  zx_status_t DisableController() { return ufs_->DisableHostController(); }
  zx_status_t EnableController() { return ufs_->EnableHostController(); }

  // Helper functions for accessing private functions.
  zx::result<> SendCommand(uint8_t slot, TransferRequestDescriptorDataDirection ddir,
                           uint16_t resp_offset, uint16_t resp_len, uint16_t prdt_offset,
                           uint16_t prdt_len, bool sync);

  // Map the data vmo to the address space and assign physical addresses. Currently, it only
  // supports 8KB vmo. So, we get two physical addresses.
  zx::result<std::array<zx_paddr_t, 2>> MapAndGetPhysicalAddress(uint32_t option,
                                                                 zx::unowned_vmo &vmo,
                                                                 fzl::VmoMapper &mapper,
                                                                 zx::pmt &pmt, uint64_t offset_vmo,
                                                                 uint64_t length);

 protected:
  std::shared_ptr<zx_device> fake_root_;
  zx_device *device_;
  std::unique_ptr<ufs_mock_device::UfsMockDevice> mock_device_;
  Ufs *ufs_;
};

}  // namespace ufs

#endif  // SRC_DEVICES_BLOCK_DRIVERS_UFS_TEST_UNIT_LIB_H_
