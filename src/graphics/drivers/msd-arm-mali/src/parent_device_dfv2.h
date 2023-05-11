// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DRIVERS_MSD_ARM_MALI_SRC_PARENT_DEVICE_DFV2_H_
#define SRC_GRAPHICS_DRIVERS_MSD_ARM_MALI_SRC_PARENT_DEVICE_DFV2_H_

#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <fidl/fuchsia.scheduler/cpp/wire.h>
#include <lib/driver/incoming/cpp/namespace.h>
#include <lib/fdf/cpp/channel.h>

#include <chrono>
#include <memory>

#include "magma_util/dlog.h"
#include "magma_util/short_macros.h"
#include "magma_util/status.h"
#include "parent_device.h"
#include "platform_interrupt.h"
#include "platform_mmio.h"

class ParentDeviceDFv2 : public ParentDevice {
 public:
  explicit ParentDeviceDFv2(
      std::shared_ptr<fdf::Namespace> incoming,
      fidl::WireSyncClient<fuchsia_hardware_platform_device::Device> pdev,
      fidl::WireSyncClient<fuchsia_scheduler::ProfileProvider> profile_provider);

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

  static std::unique_ptr<ParentDeviceDFv2> Create(std::shared_ptr<fdf::Namespace> incoming);

 private:
  std::shared_ptr<fdf::Namespace> incoming_;
  fidl::WireSyncClient<fuchsia_hardware_platform_device::Device> pdev_;
  fidl::WireSyncClient<fuchsia_scheduler::ProfileProvider> profile_provider_;
};

#endif  // SRC_GRAPHICS_DRIVERS_MSD_ARM_MALI_SRC_PARENT_DEVICE_DFV2_H_
