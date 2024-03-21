// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_CPU_DRIVERS_AML_CPU_AML_CPU_V1_H_
#define SRC_DEVICES_CPU_DRIVERS_AML_CPU_AML_CPU_V1_H_

#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.hardware.clock/cpp/wire.h>
#include <fidl/fuchsia.hardware.cpu.ctrl/cpp/wire.h>
#include <fidl/fuchsia.hardware.power/cpp/wire.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/inspect/cpp/inspector.h>

#include <ddktl/device.h>
#include <ddktl/protocol/empty-protocol.h>
#include <fbl/macros.h>
#include <soc/aml-common/aml-cpu-metadata.h>

#include "src/devices/cpu/drivers/aml-cpu/aml-cpu.h"

namespace amlogic_cpu {

class AmlCpuV1;
using DeviceType = ddk::Device<AmlCpuV1, ddk::Messageable<fuchsia_hardware_cpu_ctrl::Device>::Mixin,
                               ddk::AutoSuspendable>;

class AmlCpuV1 : public DeviceType, public ddk::EmptyProtocol<ZX_PROTOCOL_CPU_CTRL> {
 public:
  DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(AmlCpuV1);
  explicit AmlCpuV1(zx_device_t* parent, const std::vector<operating_point_t>& operating_points,
                    const perf_domain_t& perf_domain)
      : DeviceType(parent), aml_cpu_(operating_points, perf_domain) {}
  zx_status_t Init(const AmlCpuConfiguration& config);

  // Implements DDK Device Ops
  void DdkRelease();

  zx_status_t DdkConfigureAutoSuspend(bool enable, uint8_t requested_sleep_state) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  // Fidl server interface implementation.
  void GetOperatingPointInfo(GetOperatingPointInfoRequestView request,
                             GetOperatingPointInfoCompleter::Sync& completer) override {
    aml_cpu_.GetOperatingPointInfo(request, completer);
  }
  void SetCurrentOperatingPoint(SetCurrentOperatingPointRequestView request,
                                SetCurrentOperatingPointCompleter::Sync& completer) override {
    aml_cpu_.SetCurrentOperatingPoint(request, completer);
  }
  void GetCurrentOperatingPoint(GetCurrentOperatingPointCompleter::Sync& completer) override {
    aml_cpu_.GetCurrentOperatingPoint(completer);
  }
  void GetOperatingPointCount(GetOperatingPointCountCompleter::Sync& completer) override {
    aml_cpu_.GetOperatingPointCount(completer);
  }
  void GetNumLogicalCores(GetNumLogicalCoresCompleter::Sync& completer) override {
    aml_cpu_.GetNumLogicalCores(completer);
  }
  void GetLogicalCoreId(GetLogicalCoreIdRequestView request,
                        GetLogicalCoreIdCompleter::Sync& completer) override {
    aml_cpu_.GetLogicalCoreId(request, completer);
  }

  // Used by the unit test to access the device.
  AmlCpu& aml_cpu_for_testing() { return aml_cpu_; }

 private:
  AmlCpu aml_cpu_;
};

zx_status_t CreateV1Domains(void* context, zx_device_t* parent);

}  // namespace amlogic_cpu

#endif  // SRC_DEVICES_CPU_DRIVERS_AML_CPU_AML_CPU_V1_H_
