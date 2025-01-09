// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DRIVERS_MSD_ARM_MALI_SRC_PARENT_DEVICE_DFV2_H_
#define SRC_GRAPHICS_DRIVERS_MSD_ARM_MALI_SRC_PARENT_DEVICE_DFV2_H_

#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <lib/driver/incoming/cpp/namespace.h>
#include <lib/fdf/cpp/channel.h>
#include <lib/magma/platform/platform_interrupt.h>
#include <lib/magma/platform/platform_mmio.h>
#include <lib/magma/platform/zircon/zircon_platform_device_dfv2.h>
#include <lib/magma/util/dlog.h>
#include <lib/magma/util/short_macros.h>
#include <lib/magma/util/status.h>

#include <chrono>
#include <memory>

#include "parent_device.h"
#include "src/graphics/drivers/msd-arm-mali/config.h"

class ParentDeviceDFv2 : public ParentDevice {
 public:
  explicit ParentDeviceDFv2(std::shared_ptr<fdf::Namespace> incoming,
                            fidl::WireSyncClient<fuchsia_hardware_platform_device::Device> pdev,
                            config::Config config);

  ~ParentDeviceDFv2() override { DLOG("ParentDevice dtor"); }

  bool SetThreadRole(const char* role_name) override;
  zx::bti GetBusTransactionInitiator() override;

  // Map an MMIO listed at |index| in the platform device
  std::unique_ptr<magma::PlatformMmio> CpuMapMmio(
      unsigned int index, magma::PlatformMmio::CachePolicy cache_policy) override;

  // Register an interrupt listed at |index| in the platform device.
  std::unique_ptr<magma::PlatformInterrupt> RegisterInterrupt(unsigned int index) override;

  zx::result<fdf::ClientEnd<fuchsia_hardware_gpu_mali::ArmMali>> ConnectToMaliRuntimeProtocol()
      override;

  bool suspend_enabled() override { return config_.enable_suspend(); }

  std::shared_ptr<fdf::Namespace> incoming() override { return incoming_; }
  zx::result<std::vector<fdf_power::PowerElementConfiguration>> GetPowerConfiguration() override;

  static std::unique_ptr<ParentDeviceDFv2> Create(std::shared_ptr<fdf::Namespace> incoming,
                                                  config::Config config);

  magma::PlatformDevice* GetPlatformDevice() override { return &pdev_; }

 private:
  std::shared_ptr<fdf::Namespace> incoming_;
  magma::ZirconPlatformDeviceDfv2 pdev_;
  config::Config config_;
};

#endif  // SRC_GRAPHICS_DRIVERS_MSD_ARM_MALI_SRC_PARENT_DEVICE_DFV2_H_
