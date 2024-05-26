// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_CPU_DRIVERS_AML_CPU_LEGACY_AML_CPU_H_
#define SRC_DEVICES_CPU_DRIVERS_AML_CPU_LEGACY_AML_CPU_H_

#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.hardware.cpu.ctrl/cpp/wire.h>
#include <fidl/fuchsia.hardware.thermal/cpp/wire.h>
#include <lib/inspect/cpp/inspector.h>
#include <lib/mmio/mmio.h>

#include <mutex>

#include <ddktl/device.h>
#include <ddktl/protocol/empty-protocol.h>
#include <fbl/macros.h>

namespace amlogic_cpu {

namespace fuchsia_cpuctrl = fuchsia_hardware_cpu_ctrl;
namespace fuchsia_thermal = fuchsia_hardware_thermal;

class AmlCpu;
using DeviceType =
    ddk::Device<AmlCpu, ddk::Messageable<fuchsia_cpuctrl::Device>::Mixin, ddk::AutoSuspendable>;

class AmlCpu : public DeviceType, public ddk::EmptyProtocol<ZX_PROTOCOL_CPU_CTRL> {
 public:
  DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(AmlCpu);
  AmlCpu(zx_device_t* device, fidl::WireSyncClient<fuchsia_thermal::Device> thermal_client,
         size_t power_domain_index, uint32_t cluster_core_count, uint8_t relative_performance)
      : DeviceType(device),
        thermal_client_(std::move(thermal_client)),
        power_domain_index_(power_domain_index),
        cluster_core_count_(cluster_core_count),
        relative_performance_(relative_performance),
        current_operating_point_(fuchsia_cpuctrl::wire::kDeviceOperatingPointP0) {}

  static zx_status_t Create(void* context, zx_device_t* device);

  // Implements DDK Device Ops
  void DdkRelease();

  zx_status_t SetCurrentOperatingPointInternal(uint32_t requested_opp, uint32_t* out_opp);
  zx_status_t DdkConfigureAutoSuspend(bool enable, uint8_t requested_sleep_state);

  // Fidl server interface implementation.
  void GetOperatingPointInfo(GetOperatingPointInfoRequestView request,
                             GetOperatingPointInfoCompleter::Sync& completer) override;
  void SetCurrentOperatingPoint(SetCurrentOperatingPointRequestView request,
                                SetCurrentOperatingPointCompleter::Sync& completer) override;
  void GetCurrentOperatingPoint(GetCurrentOperatingPointCompleter::Sync& completer) override;
  void GetOperatingPointCount(GetOperatingPointCountCompleter::Sync& completer) override;
  void GetNumLogicalCores(GetNumLogicalCoresCompleter::Sync& completer) override;
  void GetLogicalCoreId(GetLogicalCoreIdRequestView request,
                        GetLogicalCoreIdCompleter::Sync& completer) override;
  void GetDomainId(GetDomainIdCompleter::Sync& completer) override;
  void GetRelativePerformance(GetRelativePerformanceCompleter::Sync& completer) override;

  // Set CpuInfo in inspect.
  void SetCpuInfo(uint32_t cpu_version_packed);

  // Accessor
  uint32_t ClusterCoreCount() const { return cluster_core_count_; }
  size_t PowerDomainIndex() const { return power_domain_index_; }

 private:
  zx_status_t GetThermalOperatingPoints(fuchsia_thermal::wire::OperatingPoint* out);
  fidl::WireSyncClient<fuchsia_thermal::Device> thermal_client_;
  size_t power_domain_index_;
  uint32_t cluster_core_count_;
  const uint8_t relative_performance_;

  std::mutex lock_;
  uint32_t current_operating_point_ __TA_GUARDED(lock_);

 protected:
  inspect::Inspector inspector_;
  inspect::Node cpu_info_ = inspector_.GetRoot().CreateChild("cpu_info_service");
};

}  // namespace amlogic_cpu

#endif  // SRC_DEVICES_CPU_DRIVERS_AML_CPU_LEGACY_AML_CPU_H_
