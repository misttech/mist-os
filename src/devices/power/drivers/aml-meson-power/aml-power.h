// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_POWER_DRIVERS_AML_MESON_POWER_AML_POWER_H_
#define SRC_DEVICES_POWER_DRIVERS_AML_MESON_POWER_AML_POWER_H_

#include <fidl/fuchsia.hardware.pwm/cpp/wire.h>
#include <fidl/fuchsia.hardware.vreg/cpp/wire.h>
#include <fuchsia/hardware/powerimpl/cpp/banjo.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/zx/result.h>
#include <threads.h>

#include <optional>
#include <vector>

#include <soc/aml-common/aml-power.h>

namespace power {

class AmlPower : public fdf::DriverBase, public ddk::PowerImplProtocol<AmlPower> {
 public:
  class DomainInfo {
   public:
    static constexpr int kInvalidIndex = -1;

    explicit DomainInfo(std::optional<fidl::WireSyncClient<fuchsia_hardware_pwm::Pwm>> pwm,
                        std::vector<aml_voltage_table_t> voltage_table,
                        voltage_pwm_period_ns_t pwm_period)
        : pwm(std::move(pwm)), voltage_table(std::move(voltage_table)), pwm_period(pwm_period) {}

    explicit DomainInfo(std::optional<fidl::WireSyncClient<fuchsia_hardware_vreg::Vreg>> vreg)
        : vreg(std::move(vreg)) {}

    std::optional<fidl::WireSyncClient<fuchsia_hardware_pwm::Pwm>> pwm;
    std::optional<fidl::WireSyncClient<fuchsia_hardware_vreg::Vreg>> vreg;
    std::vector<aml_voltage_table_t> voltage_table;
    voltage_pwm_period_ns_t pwm_period;
    int current_voltage_index = kInvalidIndex;
  };

  static constexpr std::string_view kDriverName = "aml-power";
  static constexpr std::string_view kChildNodeName = "power-impl";
  static constexpr std::string_view kPwmPrimaryParentName = "pwm-primary";
  static constexpr std::string_view kVregPwmLittleParentName = "vreg-pwm-little";
  static constexpr std::string_view kVregPwmBigParentName = "vreg-pwm-big";

  AmlPower(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase(kDriverName, std::move(start_args), std::move(driver_dispatcher)) {}

  // fdf::DriverBase implementation.
  zx::result<> Start() override;

  zx_status_t PowerImplGetPowerDomainStatus(uint32_t index, power_domain_status_t* out_status);
  zx_status_t PowerImplEnablePowerDomain(uint32_t index);
  zx_status_t PowerImplDisablePowerDomain(uint32_t index);
  zx_status_t PowerImplGetSupportedVoltageRange(uint32_t index, uint32_t* min_voltage,
                                                uint32_t* max_voltage);
  zx_status_t PowerImplRequestVoltage(uint32_t index, uint32_t voltage, uint32_t* actual_voltage);
  zx_status_t PowerImplGetCurrentVoltage(uint32_t index, uint32_t* current_voltage);
  zx_status_t PowerImplWritePmicCtrlReg(uint32_t index, uint32_t addr, uint32_t value);
  zx_status_t PowerImplReadPmicCtrlReg(uint32_t index, uint32_t addr, uint32_t* value);

 private:
  zx_status_t GetTargetIndex(const fidl::WireSyncClient<fuchsia_hardware_pwm::Pwm>& pwm,
                             uint32_t u_volts, const DomainInfo& domain, uint32_t* target_index);
  zx_status_t GetTargetIndex(const fidl::WireSyncClient<fuchsia_hardware_vreg::Vreg>& vreg,
                             uint32_t u_volts, const DomainInfo& domain, uint32_t* target_index);
  zx_status_t Update(const fidl::WireSyncClient<fuchsia_hardware_pwm::Pwm>& pwm, DomainInfo& domain,
                     uint32_t target_idx);
  zx_status_t Update(const fidl::WireSyncClient<fuchsia_hardware_vreg::Vreg>& vreg,
                     DomainInfo& domain, uint32_t target_idx);

  template <class ProtocolClient>
  zx_status_t RequestVoltage(const ProtocolClient& pwm, uint32_t u_volts, DomainInfo& domain);

  std::vector<DomainInfo> domain_info_;

  compat::SyncInitializedDeviceServer compat_server_;
  fidl::ClientEnd<fuchsia_driver_framework::NodeController> child_;
  compat::BanjoServer banjo_server_{ZX_PROTOCOL_POWER_IMPL, this, &power_impl_protocol_ops_};
};

}  // namespace power

#endif  // SRC_DEVICES_POWER_DRIVERS_AML_MESON_POWER_AML_POWER_H_
