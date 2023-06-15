// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#define MAGMA_DLOG_ENABLE 1

#include <lib/ddk/device.h>

#include <gtest/gtest.h>

#include "helper/platform_device_helper.h"
#include "helper/platform_msd_device_helper.h"
#include "magma_util/dlog.h"

namespace {
std::unique_ptr<magma::PlatformDevice> platform_device_s;
void* test_device_s;
}  // namespace

magma::PlatformDevice* TestPlatformDevice::GetInstance() { return platform_device_s.get(); }

msd::DeviceHandle* GetTestDeviceHandle() {
  return reinterpret_cast<msd::DeviceHandle*>(test_device_s);
}

zx_status_t magma_indriver_test(zx_device_t* device) {
  MAGMA_DLOG("running magma unit tests");
  platform_device_s = magma::PlatformDevice::Create(device);
  test_device_s = device;
  const int kArgc = 1;
  const char* argv[kArgc + 1] = {"magma_indriver_test"};
  testing::InitGoogleTest(const_cast<int*>(&kArgc), const_cast<char**>(argv));

  printf("[DRV START=]\n");
  zx_status_t status = RUN_ALL_TESTS() == 0 ? ZX_OK : ZX_ERR_INTERNAL;
  printf("[DRV END===]\n[==========]\n");
  return status;
}
