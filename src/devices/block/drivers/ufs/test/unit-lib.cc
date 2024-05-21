// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "unit-lib.h"

#include <lib/fake-bti/bti.h>

#include <memory>

#include "fuchsia/hardware/block/driver/cpp/banjo.h"

namespace ufs {

void UfsTest::SetUp() {
  fake_root_ = MockDevice::FakeRootParent();

  // Set up an interrupt.
  zx::interrupt irq;
  ASSERT_OK(zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &irq));
  zx::interrupt irq_duplicated;
  ASSERT_OK(irq.duplicate(ZX_RIGHT_SAME_RIGHTS, &irq_duplicated));

  mock_device_ = std::make_unique<ufs_mock_device::UfsMockDevice>(std::move(irq));
  ASSERT_OK(mock_device_->AddLun(0));

  // Set up the driver.
  auto driver = std::make_unique<Ufs>(fake_root_.get(), ddk::Pci{}, mock_device_->GetMmioBuffer(),
                                      fuchsia_hardware_pci::InterruptMode::kMsiX,
                                      std::move(irq_duplicated), mock_device_->GetFakeBti());
  driver->SetHostControllerCallback(Ufs::NotifyEventCallback);
  ASSERT_OK(driver->AddDevice());
  [[maybe_unused]] auto unused = driver.release();

  device_ = fake_root_->GetLatestChild();
  ufs_ = device_->GetDeviceContext<Ufs>();
}

void UfsTest::RunInit() {
  device_->InitOp();
  ASSERT_OK(device_->WaitUntilInitReplyCalled(zx::time::infinite()));
  ASSERT_OK(device_->InitReplyCallStatus());
}

void UfsTest::TearDown() {
  device_async_remove(device_);
  EXPECT_OK(mock_ddk::ReleaseFlaggedDevices(device_));
}

zx::result<> UfsTest::FillDescriptorAndSendRequest(uint8_t slot, DataDirection ddir,
                                                   uint16_t resp_offset, uint16_t resp_len,
                                                   uint16_t prdt_offset,
                                                   uint16_t prdt_entry_count) {
  return ufs_->GetTransferRequestProcessor().FillDescriptorAndSendRequest(
      slot, ddir, resp_offset, resp_len, prdt_offset, prdt_entry_count);
}

zx::result<> UfsTest::MapVmo(zx::unowned_vmo &vmo, fzl::VmoMapper &mapper, uint64_t offset,
                             uint64_t length) {
  if (zx_status_t status = mapper.Map(*vmo, offset, length); status != ZX_OK) {
    zxlogf(ERROR, "Failed to map IO buffer: %s", zx_status_get_string(status));
    return zx::error(status);
  }
  return zx::ok();
}

uint8_t UfsTest::GetSlotStateCount(SlotState slot_state) {
  uint8_t count = 0;
  for (uint8_t slot_num = 0;
       slot_num < ufs_->GetTransferRequestProcessor().request_list_.GetSlotCount(); ++slot_num) {
    auto &slot = ufs_->GetTransferRequestProcessor().request_list_.GetSlot(slot_num);
    if (slot.state == slot_state) {
      ++count;
    }
  }
  return count;
}

zx::result<uint32_t> UfsTest::ReadAttribute(Attributes attribute, uint8_t index) {
  return ufs_->GetDeviceManager().ReadAttribute(attribute, index);
}

zx::result<> UfsTest::WriteAttribute(Attributes attribute, uint32_t value, uint8_t index) {
  return ufs_->GetDeviceManager().WriteAttribute(attribute, value, index);
}

}  // namespace ufs
