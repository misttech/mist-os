// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_POWER_DRIVERS_AML_PWM_REGULATOR_AML_PWM_REGULATOR_H_
#define SRC_DEVICES_POWER_DRIVERS_AML_PWM_REGULATOR_AML_PWM_REGULATOR_H_

#include <fidl/fuchsia.hardware.pwm/cpp/wire.h>
#include <fidl/fuchsia.hardware.vreg/cpp/wire.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/component/cpp/driver_base.h>

#include <fbl/alloc_checker.h>
#include <soc/aml-common/aml-pwm-regs.h>

namespace aml_pwm_regulator {

using fuchsia_hardware_vreg::wire::VregMetadata;

class AmlPwmRegulatorDriver;

class AmlPwmRegulator : public fidl::WireServer<fuchsia_hardware_vreg::Vreg> {
 public:
  explicit AmlPwmRegulator(const VregMetadata& vreg_range,
                           fidl::WireSyncClient<fuchsia_hardware_pwm::Pwm> pwm_proto_client,
                           AmlPwmRegulatorDriver* driver);
  static zx::result<std::unique_ptr<AmlPwmRegulator>> Create(const VregMetadata& metadata,
                                                             AmlPwmRegulatorDriver* driver);

  // Vreg Implementation.
  void SetVoltageStep(SetVoltageStepRequestView request,
                      SetVoltageStepCompleter::Sync& completer) override;
  void GetVoltageStep(GetVoltageStepCompleter::Sync& completer) override;
  void GetRegulatorParams(GetRegulatorParamsCompleter::Sync& completer) override;
  void Enable(EnableCompleter::Sync& completer) override;
  void Disable(DisableCompleter::Sync& completer) override;

 private:
  const std::string name_;
  uint32_t min_voltage_uv_;
  uint32_t voltage_step_uv_;
  uint32_t num_steps_;

  uint32_t current_step_;

  fidl::WireSyncClient<fuchsia_hardware_pwm::Pwm> pwm_proto_client_;
  compat::SyncInitializedDeviceServer compat_server_;

  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;
  fidl::ServerBindingGroup<fuchsia_hardware_vreg::Vreg> bindings_;
};

class AmlPwmRegulatorDriver : public fdf::DriverBase {
 public:
  AmlPwmRegulatorDriver(fdf::DriverStartArgs start_args,
                        fdf::UnownedSynchronizedDispatcher driver_dispatcher);

  zx::result<> Start() override;

 private:
  friend class AmlPwmRegulator;

  std::unique_ptr<AmlPwmRegulator> regulators_;
};

}  // namespace aml_pwm_regulator

#endif  // SRC_DEVICES_POWER_DRIVERS_AML_PWM_REGULATOR_AML_PWM_REGULATOR_H_
