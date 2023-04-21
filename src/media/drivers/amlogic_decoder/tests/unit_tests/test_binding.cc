// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/media/drivers/amlogic_decoder/device_ctx.h"

namespace amlogic_decoder {
namespace test {

TEST(Binding, Destruction) {
  DriverCtx driver_ctx;
  std::shared_ptr<MockDevice> root = MockDevice::FakeRootParent();

  auto device = std::make_unique<DeviceCtx>(&driver_ctx, root.get());
  EXPECT_EQ(ZX_OK, device->Bind());
  (void)device.release();
  root.reset();
}

TEST(Binding, Suspend) {
  DriverCtx driver_ctx;
  std::shared_ptr<MockDevice> root = MockDevice::FakeRootParent();

  auto device = std::make_unique<DeviceCtx>(&driver_ctx, root.get());
  EXPECT_EQ(ZX_OK, device->Bind());
  ASSERT_EQ(1u, root->child_count());
  auto* child = root->GetLatestChild();
  ddk::SuspendTxn txn(device->zxdev(), 0, false, DEVICE_SUSPEND_REASON_REBOOT);
  device->DdkSuspend(std::move(txn));
  child->WaitUntilSuspendReplyCalled();

  (void)device.release();
  root.reset();
}

}  // namespace test
}  // namespace amlogic_decoder
