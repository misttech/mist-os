// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_POWER_DRIVERS_AML_PWM_REGULATOR_AML_PWM_REGULATOR_H_
#define SRC_DEVICES_POWER_DRIVERS_AML_PWM_REGULATOR_AML_PWM_REGULATOR_H_

#include <fidl/fuchsia.hardware.pwm/cpp/wire.h>
#include <fidl/fuchsia.hardware.vreg/cpp/wire.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/debug.h>

#include <ddktl/device.h>
#include <fbl/alloc_checker.h>
#include <soc/aml-common/aml-pwm-regs.h>

namespace aml_pwm_regulator {

class AmlPwmRegulator;
using AmlPwmRegulatorType = ddk::Device<AmlPwmRegulator, ddk::Initializable>;
using fuchsia_hardware_vreg::wire::PwmVregMetadataEntry;

class AmlPwmRegulator : public AmlPwmRegulatorType,
                        public fidl::WireServer<fuchsia_hardware_vreg::Vreg> {
 public:
  explicit AmlPwmRegulator(zx_device_t* parent, const PwmVregMetadataEntry& vreg_range,
                           fidl::WireSyncClient<fuchsia_hardware_pwm::Pwm> pwm_proto_client)
      : AmlPwmRegulatorType(parent),
        pwm_index_(vreg_range.pwm_index()),
        period_ns_(vreg_range.period_ns()),
        min_voltage_uv_(vreg_range.min_voltage_uv()),
        voltage_step_uv_(vreg_range.voltage_step_uv()),
        num_steps_(vreg_range.num_steps()),
        current_step_(vreg_range.num_steps()),
        pwm_proto_client_(std::move(pwm_proto_client)) {}
  static zx_status_t Create(void* ctx, zx_device_t* parent);

  // Device Protocol Implementation
  void DdkInit(ddk::InitTxn txn);
  void DdkRelease() { delete this; }

  // Vreg Implementation.
  void SetVoltageStep(SetVoltageStepRequestView request,
                      SetVoltageStepCompleter::Sync& completer) override;
  void GetVoltageStep(GetVoltageStepCompleter::Sync& completer) override;
  void GetRegulatorParams(GetRegulatorParamsCompleter::Sync& completer) override;

 private:
  friend class FakePwmRegulator;

  uint32_t pwm_index_;
  uint32_t period_ns_;
  uint32_t min_voltage_uv_;
  uint32_t voltage_step_uv_;
  uint32_t num_steps_;

  uint32_t current_step_;

  fidl::WireSyncClient<fuchsia_hardware_pwm::Pwm> pwm_proto_client_;

  std::optional<component::OutgoingDirectory> outgoing_;
  fidl::ServerEnd<fuchsia_io::Directory> outgoing_server_end_;
  fidl::ServerBindingGroup<fuchsia_hardware_vreg::Vreg> bindings_;
};

}  // namespace aml_pwm_regulator

#endif  // SRC_DEVICES_POWER_DRIVERS_AML_PWM_REGULATOR_AML_PWM_REGULATOR_H_
