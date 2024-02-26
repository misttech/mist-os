// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/dsi-dw/dsi-dw.h"

#include <zircon/types.h>

#include <memory>

#include <fake-mmio-reg/fake-mmio-reg.h>
#include <gtest/gtest.h>

#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/lib/testing/predicates/status.h"

namespace dsi_dw {

namespace {

TEST(DsiDwTest, DdkLifeCycle) {
  std::shared_ptr<MockDevice> fake_parent = MockDevice::FakeRootParent();
  ddk_fake::FakeMmioRegRegion mmio_region(sizeof(uint32_t), 0x10000);

  auto dev = std::make_unique<DsiDw>(fake_parent.get(), mmio_region.GetMmioBuffer());
  EXPECT_OK(dev->DdkAdd("dw-dsi"));
  [[maybe_unused]] DsiDw* dsi_dw_ptr = dev.release();
  // TODO(https://fxbug.dev/42159897): Removed the obsolete fake_ddk.Ok() check.
  // To test Unbind and Release behavior, call UnbindOp and ReleaseOp directly.
}

}  // namespace

}  // namespace dsi_dw
