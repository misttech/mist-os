// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "unit-lib.h"

#include <lib/fake-bti/bti.h>

#include <memory>

#include "fuchsia/hardware/block/driver/cpp/banjo.h"

namespace ufs {

namespace {
// Recursively unbind and release all devices.
zx_status_t ProcessDeviceRemoval(MockDevice* device) {
  device->UnbindOp();
  // deleting children, so use a while loop:
  while (!device->children().empty()) {
    // Only stop the dispatcher before calling the final ReleaseOp.
    auto status = ProcessDeviceRemoval(device->children().back().get());
    if (status != ZX_OK) {
      return status;
    }
  }
  if (device->HasUnbindOp()) {
    zx_status_t status = device->WaitUntilUnbindReplyCalled();
    if (status != ZX_OK) {
      return status;
    }
  }

  device->ReleaseOp();
  return ZX_OK;
}
}  // namespace

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

void UfsTest::TearDown() { ProcessDeviceRemoval(device_); }

}  // namespace ufs
