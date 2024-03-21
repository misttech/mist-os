// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_POWER_POWER_MANAGER_TESTING_FAKE_DRIVER_CPU_CTRL_SERVER_H_
#define SRC_POWER_POWER_MANAGER_TESTING_FAKE_DRIVER_CPU_CTRL_SERVER_H_

#include <fidl/fuchsia.hardware.cpu.ctrl/cpp/wire.h>

#include <mutex>

namespace fake_driver {
using operating_point_t = struct operating_point {
  uint32_t freq_hz;
  uint32_t volt_uv;
};

// Protocol served to client components over devfs.
class CpuCtrlProtocolServer : public fidl::WireServer<fuchsia_hardware_cpu_ctrl::Device> {
 public:
  explicit CpuCtrlProtocolServer();

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

  void Serve(async_dispatcher_t* dispatcher,
             fidl::ServerEnd<fuchsia_hardware_cpu_ctrl::Device> server);

 private:
  fidl::ServerBindingGroup<fuchsia_hardware_cpu_ctrl::Device> bindings_;

  std::mutex lock_;
  uint32_t current_opp_ __TA_GUARDED(lock_) = 0;
  std::vector<operating_point_t> operating_points_ = {
      {static_cast<uint32_t>(2.0e9), static_cast<uint32_t>(1.0 * 1e6)},
      {static_cast<uint32_t>(1.5e9), static_cast<uint32_t>(0.8 * 1e6)},
      {static_cast<uint32_t>(1.5e9), static_cast<uint32_t>(0.7 * 1e6)}};
};

}  // namespace fake_driver

#endif  // SRC_POWER_POWER_MANAGER_TESTING_FAKE_DRIVER_CPU_CTRL_SERVER_H_
