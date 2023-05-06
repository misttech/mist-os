// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DRIVERS_MSD_ARM_MALI_SRC_PARENT_DEVICE_DFV1_H_
#define SRC_GRAPHICS_DRIVERS_MSD_ARM_MALI_SRC_PARENT_DEVICE_DFV1_H_

#include <lib/ddk/device.h>
#include <lib/device-protocol/pdev-fidl.h>

#include "parent_device.h"

// zx_device_t* == msd::DeviceHandle*.
msd::DeviceHandle* ZxDeviceToDeviceHandle(zx_device_t* device);

class ParentDeviceDFv1 : public ParentDevice {
 public:
  explicit ParentDeviceDFv1(zx_device_t* parent, ddk::PDevFidl pdev)
      : parent_(parent), pdev_(std::move(pdev)) {}

  virtual ~ParentDeviceDFv1() override { DLOG("ParentDevice dtor"); }

  bool SetThreadRole(const char* role_name) override;
  zx::bti GetBusTransactionInitiator() override;

  // Map an MMIO listed at |index| in the platform device
  std::unique_ptr<magma::PlatformMmio> CpuMapMmio(
      unsigned int index, magma::PlatformMmio::CachePolicy cache_policy) override;

  // Register an interrupt listed at |index| in the platform device.
  std::unique_ptr<magma::PlatformInterrupt> RegisterInterrupt(unsigned int index) override;

  zx_status_t ConnectRuntimeProtocol(const char* service_name, const char* name,
                                     fdf::Channel server_end) override;

 private:
  zx_device_t* parent_;
  ddk::PDevFidl pdev_;
};

#endif  // SRC_GRAPHICS_DRIVERS_MSD_ARM_MALI_SRC_PARENT_DEVICE_DFV1_H_
