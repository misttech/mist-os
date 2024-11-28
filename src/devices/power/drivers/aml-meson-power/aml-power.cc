// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-power.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/platform-defs.h>
#include <lib/device-protocol/pdev-fidl.h>

#include <algorithm>
#include <cstdint>
#include <memory>
#include <optional>
#include <vector>

#include <fbl/alloc_checker.h>
#include <soc/aml-common/aml-pwm-regs.h>

namespace power {

namespace {

// Sleep for 200 microseconds inorder to let the voltage change
// take effect. Source: Amlogic SDK.
constexpr uint32_t kVoltageSettleTimeUs = 200;
// Step up or down 3 steps in the voltage table while changing
// voltage and not directly. Source: Amlogic SDK
constexpr int kMaxVoltageChangeSteps = 3;

zx_status_t InitPwmProtocolClient(const fidl::WireSyncClient<fuchsia_hardware_pwm::Pwm>& client) {
  if (!client.is_valid()) {
    // Optional fragment. See comment in AmlPower::Create.
    return ZX_OK;
  }

  auto result = client->Enable();
  if (!result.ok()) {
    zxlogf(ERROR, "%s: Could not enable PWM", __func__);
    return result.status();
  }
  if (result.value().is_error()) {
    zxlogf(ERROR, "%s: Could not enable PWM", __func__);
    return result.value().error_value();
  }
  return ZX_OK;
}

bool IsSortedDescending(const std::vector<aml_voltage_table_t>& vt) {
  for (size_t i = 0; i < vt.size() - 1; i++) {
    if (vt[i].microvolt < vt[i + 1].microvolt)
      // Bail early if we find a voltage that isn't strictly descending.
      return false;
  }
  return true;
}

uint32_t CalculateVregVoltage(const uint32_t min_uv, const uint32_t step_size_uv,
                              const uint32_t idx) {
  return min_uv + idx * step_size_uv;
}

}  // namespace

zx_status_t AmlPower::PowerImplWritePmicCtrlReg(uint32_t index, uint32_t addr, uint32_t value) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t AmlPower::PowerImplReadPmicCtrlReg(uint32_t index, uint32_t addr, uint32_t* value) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t AmlPower::PowerImplDisablePowerDomain(uint32_t index) {
  if (index >= domain_info_.size()) {
    zxlogf(ERROR, "%s: Requested Disable for a domain that doesn't exist, idx = %u", __func__,
           index);
    return ZX_ERR_OUT_OF_RANGE;
  }

  return ZX_OK;
}

zx_status_t AmlPower::PowerImplEnablePowerDomain(uint32_t index) {
  if (index >= domain_info_.size()) {
    zxlogf(ERROR, "%s: Requested Enable for a domain that doesn't exist, idx = %u", __func__,
           index);
    return ZX_ERR_OUT_OF_RANGE;
  }

  return ZX_OK;
}

zx_status_t AmlPower::PowerImplGetPowerDomainStatus(uint32_t index,
                                                    power_domain_status_t* out_status) {
  if (index >= domain_info_.size()) {
    zxlogf(ERROR,
           "%s: Requested PowerImplGetPowerDomainStatus for a domain that doesn't exist, idx = %u",
           __func__, index);
    return ZX_ERR_OUT_OF_RANGE;
  }

  if (out_status == nullptr) {
    zxlogf(ERROR, "%s: out_status must not be null", __func__);
    return ZX_ERR_INVALID_ARGS;
  }

  // All domains are always enabled.
  *out_status = POWER_DOMAIN_STATUS_ENABLED;
  return ZX_OK;
}

zx_status_t AmlPower::PowerImplGetSupportedVoltageRange(uint32_t index, uint32_t* min_voltage,
                                                        uint32_t* max_voltage) {
  if (!min_voltage || !max_voltage) {
    zxlogf(ERROR, "Need non-nullptr for min_voltage and max_voltage");
    return ZX_ERR_INVALID_ARGS;
  }

  if (index >= domain_info_.size()) {
    zxlogf(ERROR,
           "%s: Requested GetSupportedVoltageRange for a domain that doesn't exist, idx = %u",
           __func__, index);
    return ZX_ERR_OUT_OF_RANGE;
  }

  auto& domain = domain_info_[index];
  if (domain.vreg) {
    fidl::WireResult params = (*domain.vreg)->GetRegulatorParams();
    if (!params.ok()) {
      zxlogf(ERROR, "Failed to send request to get regulator params: %s", params.status_string());
      return params.status();
    }

    *min_voltage = CalculateVregVoltage(params->min_uv, params->step_size_uv, 0);
    *max_voltage = CalculateVregVoltage(params->min_uv, params->step_size_uv, params->num_steps);
    zxlogf(DEBUG, "%s: Getting %s Cluster VReg Range max = %u, min = %u", __func__,
           index ? "Little" : "Big", *max_voltage, *min_voltage);

    return ZX_OK;
  }
  if (domain.pwm) {
    // Voltage table is sorted in descending order so the minimum voltage is the last element and
    // the maximum voltage is the first element.
    *min_voltage = domain.voltage_table.back().microvolt;
    *max_voltage = domain.voltage_table.front().microvolt;
    zxlogf(DEBUG, "%s: Getting %s Cluster VReg Range max = %u, min = %u", __func__,
           index ? "Little" : "Big", *max_voltage, *min_voltage);

    return ZX_OK;
  }

  zxlogf(ERROR,
         "%s: Neither Vreg nor PWM are supported for this cluster. This should never happen.",
         __func__);
  return ZX_ERR_INTERNAL;
}

zx_status_t AmlPower::GetTargetIndex(const fidl::WireSyncClient<fuchsia_hardware_pwm::Pwm>& pwm,
                                     uint32_t u_volts, const DomainInfo& domain,
                                     uint32_t* target_index) {
  if (!target_index) {
    return ZX_ERR_INTERNAL;
  }

  // Find the largest voltage that does not exceed u_volts.
  const aml_voltage_table_t target_voltage = {.microvolt = u_volts, .duty_cycle = 0};

  const auto& target =
      std::lower_bound(domain.voltage_table.cbegin(), domain.voltage_table.cend(), target_voltage,
                       [](const aml_voltage_table_t& l, const aml_voltage_table_t& r) {
                         return l.microvolt > r.microvolt;
                       });

  if (target == domain.voltage_table.cend()) {
    zxlogf(ERROR, "%s: Could not find a voltage less than or equal to %u\n", __func__, u_volts);
    return ZX_ERR_NOT_SUPPORTED;
  }

  size_t target_idx = target - domain.voltage_table.cbegin();
  if (target_idx >= INT_MAX || target_idx >= domain.voltage_table.size()) {
    zxlogf(ERROR, "%s: voltage target index out of bounds", __func__);
    return ZX_ERR_OUT_OF_RANGE;
  }
  *target_index = static_cast<int>(target_idx);

  return ZX_OK;
}

zx_status_t AmlPower::GetTargetIndex(const fidl::WireSyncClient<fuchsia_hardware_vreg::Vreg>& vreg,
                                     uint32_t u_volts, const DomainInfo& domain,
                                     uint32_t* target_index) {
  if (!target_index) {
    return ZX_ERR_INTERNAL;
  }

  fidl::WireResult params = vreg->GetRegulatorParams();
  if (!params.ok()) {
    zxlogf(ERROR, "Failed to send request to get regulator params: %s", params.status_string());
    return params.status();
  }

  const auto min_voltage_uv = CalculateVregVoltage(params->min_uv, params->step_size_uv, 0);
  const auto max_voltage_uv =
      CalculateVregVoltage(params->min_uv, params->step_size_uv, params->num_steps);
  // Find the step value that achieves the requested voltage.
  if (u_volts < min_voltage_uv || u_volts > max_voltage_uv) {
    zxlogf(ERROR, "%s: Voltage must be between %u and %u microvolts", __func__, min_voltage_uv,
           max_voltage_uv);
    return ZX_ERR_NOT_SUPPORTED;
  }

  *target_index = (u_volts - min_voltage_uv) / params->step_size_uv;
  ZX_ASSERT(*target_index <= params->num_steps);

  return ZX_OK;
}

zx_status_t AmlPower::Update(const fidl::WireSyncClient<fuchsia_hardware_pwm::Pwm>& pwm,
                             DomainInfo& domain, uint32_t target_idx) {
  aml_pwm::mode_config on = {aml_pwm::Mode::kOn, {}};
  fuchsia_hardware_pwm::wire::PwmConfig cfg = {
      .polarity = false,
      .period_ns = domain.pwm_period,
      .duty_cycle = static_cast<float>(domain.voltage_table[target_idx].duty_cycle),
      .mode_config =
          fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(&on), sizeof(on))};
  auto result = pwm->SetConfig(cfg);
  if (!result.ok()) {
    return result.status();
  }
  if (result.value().is_error()) {
    return result.value().error_value();
  }
  usleep(kVoltageSettleTimeUs);
  domain.current_voltage_index = target_idx;
  return ZX_OK;
}

zx_status_t AmlPower::Update(const fidl::WireSyncClient<fuchsia_hardware_vreg::Vreg>& vreg,
                             DomainInfo& domain, uint32_t target_idx) {
  fidl::WireResult step = vreg->SetVoltageStep(target_idx);
  if (!step.ok()) {
    zxlogf(ERROR, "Failed to send request to set voltage step: %s", step.status_string());
    return step.status();
  }
  if (step->is_error()) {
    zxlogf(ERROR, "Failed to set voltage step: %s", zx_status_get_string(step->error_value()));
    return step->error_value();
  }
  usleep(kVoltageSettleTimeUs);
  domain.current_voltage_index = target_idx;
  return ZX_OK;
}

template <class ProtocolClient>
zx_status_t AmlPower::RequestVoltage(const ProtocolClient& client, uint32_t u_volts,
                                     DomainInfo& domain) {
  uint32_t target_idx;
  auto status = GetTargetIndex(client, u_volts, domain, &target_idx);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Could not get target index\n");
    return status;
  }

  // If this is the first time we are setting up the voltage
  // we directly set it.
  if (domain.current_voltage_index == DomainInfo::kInvalidIndex) {
    status = Update(client, domain, target_idx);
    if (status != ZX_OK) {
      zxlogf(ERROR, "%s: Could not update", __func__);
      return status;
    }
    return ZX_OK;
  }

  // Otherwise we adjust to the target voltage step by step.
  auto target_index = static_cast<int>(target_idx);
  while (domain.current_voltage_index != target_index) {
    if (domain.current_voltage_index < target_index) {
      if (domain.current_voltage_index < target_index - kMaxVoltageChangeSteps) {
        // Step up by 3 in the voltage table.
        domain.current_voltage_index += kMaxVoltageChangeSteps;
      } else {
        domain.current_voltage_index = target_index;
      }
    } else {
      if (domain.current_voltage_index > target_index + kMaxVoltageChangeSteps) {
        // Step down by 3 in the voltage table.
        domain.current_voltage_index -= kMaxVoltageChangeSteps;
      } else {
        domain.current_voltage_index = target_index;
      }
    }
    status = Update(client, domain, domain.current_voltage_index);
    if (status != ZX_OK) {
      zxlogf(ERROR, "%s: Could not update", __func__);
      return status;
    }
  }
  return ZX_OK;
}

zx_status_t AmlPower::PowerImplRequestVoltage(uint32_t index, uint32_t voltage,
                                              uint32_t* actual_voltage) {
  if (index >= domain_info_.size()) {
    zxlogf(ERROR, "%s: Requested voltage for a range that doesn't exist, idx = %u", __func__,
           index);
    return ZX_ERR_OUT_OF_RANGE;
  }

  auto& domain = domain_info_[index];
  if (domain.pwm) {
    zx_status_t st = RequestVoltage(domain.pwm.value(), voltage, domain);
    if ((st == ZX_OK) && actual_voltage) {
      *actual_voltage = domain.voltage_table[domain.current_voltage_index].microvolt;
    }
    return st;
  }

  if (domain.vreg) {
    zx_status_t st = RequestVoltage(domain.vreg.value(), voltage, domain);
    if ((st == ZX_OK) && actual_voltage) {
      fidl::WireResult params = (*domain.vreg)->GetRegulatorParams();
      if (!params.ok()) {
        zxlogf(ERROR, "Failed to send request to get regulator params: %s", params.status_string());
        return params.status();
      }

      *actual_voltage =
          CalculateVregVoltage(params->min_uv, params->step_size_uv, domain.current_voltage_index);
    }
    return st;
  }

  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t AmlPower::PowerImplGetCurrentVoltage(uint32_t index, uint32_t* current_voltage) {
  if (!current_voltage) {
    zxlogf(ERROR, "Cannot take nullptr for current_voltage");
    return ZX_ERR_INVALID_ARGS;
  }

  if (index >= domain_info_.size()) {
    zxlogf(ERROR, "%s: Requested voltage for a range that doesn't exist, idx = %u", __func__,
           index);
    return ZX_ERR_OUT_OF_RANGE;
  }

  auto& domain = domain_info_[index];
  if (domain.pwm) {
    if (domain.current_voltage_index == DomainInfo::kInvalidIndex)
      return ZX_ERR_BAD_STATE;
    *current_voltage = domain.voltage_table[domain.current_voltage_index].microvolt;
  } else if (domain.vreg) {
    fidl::WireResult params = (*domain.vreg)->GetRegulatorParams();
    if (!params.ok()) {
      zxlogf(ERROR, "Failed to send request to get regulator params: %s", params.status_string());
      return params.status();
    }

    *current_voltage =
        CalculateVregVoltage(params->min_uv, params->step_size_uv, domain.current_voltage_index);
  } else {
    return ZX_ERR_INTERNAL;
  }

  return ZX_OK;
}

void AmlPower::DdkRelease() { delete this; }

zx_status_t AmlPower::Create(void* ctx, zx_device_t* parent) {
  // Create tries to get all possible metadata and fragments. The required combination of metadata
  // and fragment is expected to be configured appropriately by the board driver. After gathering
  // all the available metadata and fragment DomainInfo for little core and big core (if exists) is
  // populated. DomainInfo vector is then used to construct AmlPower.
  auto voltage_table =
      ddk::GetMetadataArray<aml_voltage_table_t>(parent, DEVICE_METADATA_AML_VOLTAGE_TABLE);
  if (voltage_table.is_ok()) {
    if (!IsSortedDescending(*voltage_table)) {
      zxlogf(ERROR, "%s: Voltage table was not sorted in strictly descending order", __func__);
      return ZX_ERR_INTERNAL;
    }
  } else if (voltage_table.error_value() != ZX_ERR_NOT_FOUND) {
    zxlogf(ERROR, "%s: Failed to get aml voltage table, st = %d", __func__,
           voltage_table.error_value());
    return voltage_table.error_value();
  }

  zx::result<std::unique_ptr<voltage_pwm_period_ns_t>> pwm_period =
      ddk::GetMetadata<voltage_pwm_period_ns_t>(parent, DEVICE_METADATA_AML_PWM_PERIOD_NS);
  if (!pwm_period.is_ok() && pwm_period.error_value() != ZX_ERR_NOT_FOUND) {
    zxlogf(ERROR, "%s: Failed to get aml pwm period, st = %d", __func__, pwm_period.error_value());
    return pwm_period.error_value();
  }

  zx_status_t st;
  fidl::WireSyncClient<fuchsia_hardware_pwm::Pwm> primary_cluster_pwm;
  zx::result client_end =
      DdkConnectFragmentFidlProtocol<fuchsia_hardware_pwm::Service::Pwm>(parent, "pwm-primary");

  // The fragment may be optional, so we do not return on error.
  if (!client_end.is_error()) {
    zxlogf(INFO, "Connected to primary pwm");
    primary_cluster_pwm.Bind(std::move(client_end.value()));
    st = InitPwmProtocolClient(primary_cluster_pwm);
    if (st != ZX_OK) {
      zxlogf(ERROR, "%s: Failed to initialize Big Cluster PWM Client, st = %d", __func__, st);
      return st;
    }
  }

  zx::result little_cluster_vreg =
      DdkConnectFragmentFidlProtocol<fuchsia_hardware_vreg::Service::Vreg>(parent,
                                                                           "vreg-pwm-little");

  zx::result big_cluster_vreg =
      DdkConnectFragmentFidlProtocol<fuchsia_hardware_vreg::Service::Vreg>(parent, "vreg-pwm-big");

  std::vector<DomainInfo> domain_info;
  if (primary_cluster_pwm.is_valid() && voltage_table.is_ok() && pwm_period.is_ok()) {
    // For Astro.
    domain_info.emplace_back(std::move(primary_cluster_pwm), *voltage_table, *pwm_period.value());
  } else {
    // For Vim3.
    if (big_cluster_vreg.is_error() || little_cluster_vreg.is_error()) {
      zxlogf(ERROR, "Unable to connect to VReg Devices: big = %s, little = %s",
             big_cluster_vreg.is_error() ? "Success" : "Failure",
             little_cluster_vreg.is_error() ? "Success" : "Failure");
      return ZX_ERR_INTERNAL;
    }

    // TODO(b/376751395): This conflates power domain IDs with indices in the domain_info vector.
    //                    In other words domain IDs are implicitly coupled to their index in this
    //                    vector which creates a fragile mapping from power domain ID to power.
    //                    We should reconsider this.
    domain_info.emplace_back(
        fidl::WireSyncClient<fuchsia_hardware_vreg::Vreg>(std::move(big_cluster_vreg.value())));
    domain_info.emplace_back(
        fidl::WireSyncClient<fuchsia_hardware_vreg::Vreg>(std::move(little_cluster_vreg.value())));
  }

  std::unique_ptr<AmlPower> power_impl_device =
      std::make_unique<AmlPower>(parent, std::move(domain_info));

  st = power_impl_device->DdkAdd(
      ddk::DeviceAddArgs("power-impl").forward_metadata(parent, DEVICE_METADATA_POWER_DOMAINS));
  if (st != ZX_OK) {
    zxlogf(ERROR, "%s: DdkAdd failed, st = %d", __func__, st);
  }

  // Let device runner take ownership of this object.
  [[maybe_unused]] auto* dummy = power_impl_device.release();

  return st;
}

static constexpr zx_driver_ops_t aml_power_driver_ops = []() {
  zx_driver_ops_t driver_ops = {};
  driver_ops.version = DRIVER_OPS_VERSION;
  driver_ops.bind = AmlPower::Create;
  // driver_ops.run_unit_tests = run_test;  # TODO(gkalsi).
  return driver_ops;
}();

}  // namespace power

ZIRCON_DRIVER(aml_power, power::aml_power_driver_ops, "zircon", "0.1");
