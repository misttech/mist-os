// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/magma_service/msd.h>

#include <gtest/gtest.h>

#include "helper/platform_msd_device_helper.h"

TEST(MsdDevice, CreateAndDestroy) {
  auto msd_driver = msd::Driver::Create();
  ASSERT_NE(msd_driver, nullptr);

  auto msd_device = msd_driver->CreateDevice(GetTestDeviceHandle());
  ASSERT_NE(msd_device, nullptr);
  msd_device.reset();
  msd_driver.reset();
}
