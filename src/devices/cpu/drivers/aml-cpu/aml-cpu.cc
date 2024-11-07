// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-cpu.h"

#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/fit/defer.h>
#include <lib/fit/function.h>
#include <lib/mmio/mmio.h>
#include <lib/trace-engine/types.h>
#include <lib/trace/event.h>
#include <lib/trace/event_args.h>
#include <zircon/syscalls/smc.h>

#include <vector>

namespace amlogic_cpu {

namespace {
constexpr uint32_t kCpuGetDvfsTableIndexFuncId = 0x82000088;
constexpr uint64_t kDefaultClusterId = 0;

constexpr uint32_t kInitialOpp = fuchsia_hardware_cpu_ctrl::wire::kDeviceOperatingPointP0;

}  // namespace

zx_status_t GetPopularVoltageTable(const zx::resource& smc_resource, uint32_t* metadata_type) {
  if (smc_resource.is_valid()) {
    zx_smc_parameters_t smc_params = {};
    smc_params.func_id = kCpuGetDvfsTableIndexFuncId;
    smc_params.arg1 = kDefaultClusterId;

    zx_smc_result_t smc_result;
    zx_status_t status = zx_smc_call(smc_resource.get(), &smc_params, &smc_result);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "zx_smc_call failed: %s", zx_status_get_string(status));
      return status;
    }

    switch (smc_result.arg0) {
      case amlogic_cpu::OppTable1:
        *metadata_type = DEVICE_METADATA_AML_OP_1_POINTS;
        break;
      case amlogic_cpu::OppTable2:
        *metadata_type = DEVICE_METADATA_AML_OP_2_POINTS;
        break;
      case amlogic_cpu::OppTable3:
        *metadata_type = DEVICE_METADATA_AML_OP_3_POINTS;
        break;
      default:
        *metadata_type = DEVICE_METADATA_AML_OP_POINTS;
        break;
    }
    FDF_LOG(INFO, "Dvfs using table%ld.\n", smc_result.arg0);
  }

  return ZX_OK;
}

zx::result<AmlCpuConfiguration> LoadConfiguration(ddk::PDevFidl& pdev) {
  zx_status_t st;
  AmlCpuConfiguration config;

  std::optional<fdf::MmioBuffer> mmio_buffer;
  st = pdev.MapMmio(0, &mmio_buffer);
  if (st != ZX_OK) {
    FDF_LOG(ERROR, "aml-cpu: Failed to map mmio: %s", zx_status_get_string(st));
    return zx::error(st);
  }

  config.info = {};
  st = pdev.GetDeviceInfo(&config.info);
  if (st != ZX_OK) {
    FDF_LOG(ERROR, "Failed to get DeviceInfo: %s", zx_status_get_string(st));
    return zx::error(st);
  }

  zx::resource smc_resource = {};
  config.metadata_type = DEVICE_METADATA_AML_OP_POINTS;
  config.fragments_per_pf_domain = kFragmentsPerPfDomain;
  zx_off_t cpu_version_offset = kCpuVersionOffset;
  if (config.info.pid == PDEV_PID_AMLOGIC_A5) {
    st = pdev.GetSmc(0, &smc_resource);
    if (st != ZX_OK) {
      FDF_LOG(ERROR, "Failed to get smc: %s", zx_status_get_string(st));
      return zx::error(st);
    }
    st = GetPopularVoltageTable(smc_resource, &config.metadata_type);
    if (st != ZX_OK) {
      FDF_LOG(ERROR, "Failed to get popular voltage table: %s", zx_status_get_string(st));
      return zx::error(st);
    }
    config.fragments_per_pf_domain = kFragmentsPerPfDomainA5;
    cpu_version_offset = kCpuVersionOffsetA5;
  } else if (config.info.pid == PDEV_PID_AMLOGIC_A1) {
    config.fragments_per_pf_domain = kFragmentsPerPfDomainA1;
    cpu_version_offset = kCpuVersionOffsetA1;
  }

  config.cpu_version_packed = mmio_buffer->Read32(cpu_version_offset);

  config.has_div16_clients = config.fragments_per_pf_domain == kFragmentsPerPfDomain;

  // For A1, the CPU power is VDD_CORE, which share with other module.
  // The fixed voltage is 0.8v, we can't adjust it dynamically.
  config.has_power_client = config.info.pid != PDEV_PID_AMLOGIC_A1;

  return zx::ok(config);
}

std::vector<operating_point_t> PerformanceDomainOpPoints(const perf_domain_t& perf_domain,
                                                         std::vector<operating_point>& op_points) {
  std::vector<operating_point_t> pd_op_points;
  std::copy_if(op_points.begin(), op_points.end(), std::back_inserter(pd_op_points),
               [&perf_domain](const operating_point_t& op) { return op.pd_id == perf_domain.id; });

  // Order operating points from highest frequency to lowest because Operating Point 0 is the
  // fastest.
  std::sort(pd_op_points.begin(), pd_op_points.end(),
            [](const operating_point_t& a, const operating_point_t& b) {
              // Use voltage as a secondary sorting key.
              if (a.freq_hz == b.freq_hz) {
                return a.volt_uv > b.volt_uv;
              }
              return a.freq_hz > b.freq_hz;
            });

  return pd_op_points;
}

zx_status_t AmlCpu::SetCurrentOperatingPointInternal(uint32_t requested_opp, uint32_t* out_opp) {
  std::scoped_lock lock(lock_);

  if (requested_opp >= operating_points_.size()) {
    FDF_LOG(ERROR, "Requested opp is out of bounds, opp = %u\n", requested_opp);
    return ZX_ERR_OUT_OF_RANGE;
  }

  if (!out_opp) {
    FDF_LOG(ERROR, "out_opp may not be null");
    return ZX_ERR_INVALID_ARGS;
  }

  const operating_point_t& target_opp = operating_points_[requested_opp];
  const operating_point_t& initial_opp = operating_points_[current_operating_point_];

  // In the event of an error, these are used to attempt to revert the state of the parent
  // power/clock settings back to their original state.
  fit::deferred_action<std::function<void(void)>> reset_voltage;
  fit::deferred_action<std::function<void(void)>> reset_frequency;
  auto reset_voltage_func = [this, initial_opp]() {
    auto result = this->pwr_->RequestVoltage(initial_opp.volt_uv);
    if (!result.ok()) {
      FDF_LOG(ERROR, "FIDL Call Failed to restore voltage to original setting: %s",
              result.status_string());
    }
    if (result->is_error()) {
      FDF_LOG(ERROR, "Failed to restore voltage to original setting: %s",
              zx_status_get_string(result->error_value()));
    }
    uint32_t actual_voltage = result->value()->actual_voltage;
    if (actual_voltage != initial_opp.volt_uv) {
      FDF_LOG(ERROR, "Restored voltage does not match, requested = %u, got = %u",
              initial_opp.volt_uv, actual_voltage);
    }
  };

  // There is no condition under which this function will return ZX_OK but out_opp will not
  // be requested_opp so we're going to go ahead and set that up front.
  *out_opp = requested_opp;

  // TODO(b/376589801): Consider publishing this via inspect.
  FDF_LOG(DEBUG, "Scaling from %u MHz %u mV to %u MHz %u mV", initial_opp.freq_hz / 1000000,
          initial_opp.volt_uv / 1000, target_opp.freq_hz / 1000000, target_opp.volt_uv / 1000);

  if (initial_opp.freq_hz == target_opp.freq_hz && initial_opp.volt_uv == target_opp.volt_uv) {
    // Nothing to be done.
    return ZX_OK;
  }

  if (target_opp.volt_uv > initial_opp.volt_uv) {
    // If we're increasing the voltage we need to do it before setting the
    // frequency.
    ZX_ASSERT(pwr_.is_valid());
    fidl::WireResult result = pwr_->RequestVoltage(target_opp.volt_uv);
    if (!result.ok()) {
      FDF_LOG(ERROR, "Failed to send RequestVoltage request: %s", result.status_string());
      return result.error().status();
    }

    if (result->is_error()) {
      FDF_LOG(ERROR, "RequestVoltage call returned error: %s",
              zx_status_get_string(result->error_value()));
      return result->error_value();
    }

    uint32_t actual_voltage = result->value()->actual_voltage;
    if (actual_voltage != target_opp.volt_uv) {
      FDF_LOG(ERROR, "Actual voltage does not match, requested = %u, got = %u", target_opp.volt_uv,
              actual_voltage);
      reset_voltage_func();
      return ZX_ERR_INTERNAL;
    }

    // Setting the voltage was a success, arm the deferred call.
    reset_voltage = reset_voltage_func;
  }

  // Set the frequency next.
  fidl::WireResult result = cpuscaler_->SetRate(target_opp.freq_hz);
  if (!result.ok() || result->is_error()) {
    FDF_LOG(ERROR, "Could not set CPU frequency: %s", result.FormatDescription().c_str());

    if (!result.ok()) {
      return result.status();
    }
    return result->error_value();
  }

  reset_frequency = [this, initial_opp]() {
    auto result = this->cpuscaler_->SetRate(initial_opp.freq_hz);
    if (!result.ok()) {
      FDF_LOG(ERROR, "FIDL Call Failed to restore frequency to original setting: %s",
              result.status_string());
    }
    if (result->is_error()) {
      FDF_LOG(ERROR, "Failed to restore frequency to original setting: %s",
              zx_status_get_string(result->error_value()));
    }
  };

  // If we're decreasing the voltage, then we do it after the frequency has been
  // reduced to avoid undervolt conditions.
  if (target_opp.volt_uv < initial_opp.volt_uv) {
    ZX_ASSERT(pwr_.is_valid());
    fidl::WireResult result = pwr_->RequestVoltage(target_opp.volt_uv);
    if (!result.ok()) {
      FDF_LOG(ERROR, "Failed to send RequestVoltage request: %s", result.status_string());
      return result.error().status();
    }

    if (result->is_error()) {
      FDF_LOG(ERROR, "RequestVoltage call returned error: %s",
              zx_status_get_string(result->error_value()));
      return result->error_value();
    }

    uint32_t actual_voltage = result->value()->actual_voltage;
    if (actual_voltage != target_opp.volt_uv) {
      FDF_LOG(ERROR,
              "Failed to set cpu voltage, requested = %u, got = %u. "
              "Voltage and frequency mismatch!",
              target_opp.volt_uv, actual_voltage);
      reset_voltage_func();
      return ZX_ERR_INTERNAL;
    }
  }

  FDF_LOG(DEBUG, "switch opp from %u to %u success!\n", current_operating_point_, requested_opp);

  current_operating_point_ = requested_opp;

  // Cancel any deferred unwind calls.
  reset_voltage.cancel();
  reset_frequency.cancel();

  TRACE_COUNTER("dvfs", "cpu_freq", GetDomainId(), "frequency", TA_INT64(target_opp.freq_hz),
                "voltage", TA_INT64(target_opp.volt_uv));

  return ZX_OK;
}

zx_status_t AmlCpu::Init(fidl::ClientEnd<fuchsia_hardware_clock::Clock> plldiv16,
                         fidl::ClientEnd<fuchsia_hardware_clock::Clock> cpudiv16,
                         fidl::ClientEnd<fuchsia_hardware_clock::Clock> cpuscaler,
                         fidl::ClientEnd<fuchsia_hardware_power::Device> pwr) {
  cpuscaler_.Bind(std::move(cpuscaler));

  if (plldiv16.is_valid()) {
    plldiv16_.Bind(std::move(plldiv16));

    fidl::WireResult result = plldiv16_->Enable();
    if (!result.ok()) {
      FDF_LOG(ERROR, "Failed to send request to enable plldiv16: %s", result.status_string());
      return result.status();
    }
    if (result->is_error()) {
      FDF_LOG(ERROR, "Failed to enable plldiv16: %s", zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }

  if (cpudiv16.is_valid()) {
    cpudiv16_.Bind(std::move(cpudiv16));

    fidl::WireResult result = cpudiv16_->Enable();
    if (!result.ok()) {
      FDF_LOG(ERROR, "Failed to send request to enable cpudiv16: %s", result.status_string());
      return result.status();
    }
    if (result->is_error()) {
      FDF_LOG(ERROR, "Failed to enable cpudiv16: %s", zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }

  if (pwr.is_valid()) {
    pwr_.Bind(std::move(pwr));

    fidl::WireResult voltage_range_result = pwr_->GetSupportedVoltageRange();
    if (!voltage_range_result.ok()) {
      FDF_LOG(ERROR, "Failed to send GetSupportedVoltageRange request: %s",
              voltage_range_result.status_string());
      return voltage_range_result.status();
    }

    if (voltage_range_result->is_error()) {
      FDF_LOG(ERROR, "GetSupportedVoltageRange returned error: %s",
              zx_status_get_string(voltage_range_result->error_value()));
      return voltage_range_result->error_value();
    }

    uint32_t max_voltage = voltage_range_result->value()->max;
    uint32_t min_voltage = voltage_range_result->value()->min;

    fidl::WireResult register_result = pwr_->RegisterPowerDomain(min_voltage, max_voltage);
    if (!register_result.ok()) {
      FDF_LOG(ERROR, "Failed to send RegisterPowerDomain request: %s",
              register_result.status_string());
      return voltage_range_result.status();
    }

    if (register_result->is_error()) {
      FDF_LOG(ERROR, "RegisterPowerDomain returned error: %s",
              zx_status_get_string(register_result->error_value()));
      return register_result->error_value();
    }
  }

  uint32_t actual;
  // Returns ZX_ERR_OUT_OF_RANGE if `operating_points_` is empty.
  zx_status_t result = SetCurrentOperatingPointInternal(kInitialOpp, &actual);

  if (result != ZX_OK) {
    FDF_LOG(ERROR, "Failed to set initial opp, st = %d", result);
    return result;
  }

  if (actual != kInitialOpp) {
    FDF_LOG(ERROR, "Failed to set initial opp, requested = %u, actual = %u", kInitialOpp, actual);
    return ZX_ERR_INTERNAL;
  }

  return ZX_OK;
}

void AmlCpu::SetCpuInfo(uint32_t cpu_version_packed) {
  const uint8_t major_revision = (cpu_version_packed >> 24) & 0xff;
  const uint8_t minor_revision = (cpu_version_packed >> 8) & 0xff;
  const uint8_t cpu_package_id = (cpu_version_packed >> 20) & 0x0f;
  FDF_LOG(INFO, "major revision number: 0x%x", major_revision);
  FDF_LOG(INFO, "minor revision number: 0x%x", minor_revision);
  FDF_LOG(INFO, "cpu package id number: 0x%x", cpu_package_id);

  inspect_major_revision_ = cpu_info_.CreateUint("cpu_major_revision", major_revision);
  inspect_minor_revision_ = cpu_info_.CreateUint("cpu_minor_revision", minor_revision);
  inspect_package_id_ = cpu_info_.CreateUint("cpu_package_id", cpu_package_id);
}

void AmlCpu::GetOperatingPointInfo(GetOperatingPointInfoRequestView request,
                                   GetOperatingPointInfoCompleter::Sync& completer) {
  auto operating_points = GetOperatingPoints();
  if (request->opp >= operating_points.size()) {
    FDF_LOG(INFO, "Requested an operating point that's out of bounds, %u\n", request->opp);
    completer.ReplyError(ZX_ERR_OUT_OF_RANGE);
    return;
  }

  fuchsia_hardware_cpu_ctrl::wire::CpuOperatingPointInfo result;
  result.frequency_hz = operating_points[request->opp].freq_hz;
  result.voltage_uv = operating_points[request->opp].volt_uv;

  completer.ReplySuccess(result);
}

void AmlCpu::SetCurrentOperatingPoint(SetCurrentOperatingPointRequestView request,
                                      SetCurrentOperatingPointCompleter::Sync& completer) {
  uint32_t out_opp = 0;
  zx_status_t status = SetCurrentOperatingPointInternal(request->requested_opp, &out_opp);
  if (status != ZX_OK) {
    completer.ReplyError(status);
  } else {
    completer.ReplySuccess(out_opp);
  }
}

void AmlCpu::GetCurrentOperatingPoint(GetCurrentOperatingPointCompleter::Sync& completer) {
  completer.Reply(GetCurrentOperatingPoint());
}

void AmlCpu::GetOperatingPointCount(GetOperatingPointCountCompleter::Sync& completer) {
  completer.ReplySuccess(GetOperatingPointCount());
}

void AmlCpu::GetNumLogicalCores(GetNumLogicalCoresCompleter::Sync& completer) {
  completer.Reply(GetCoreCount());
}

void AmlCpu::GetLogicalCoreId(GetLogicalCoreIdRequestView request,
                              GetLogicalCoreIdCompleter::Sync& completer) {
  // Placeholder.
  completer.Reply(0);
}

void AmlCpu::GetDomainId(GetDomainIdCompleter::Sync& completer) {
  completer.Reply(perf_domain_.id);
}

void AmlCpu::GetRelativePerformance(GetRelativePerformanceCompleter::Sync& completer) {
  completer.ReplySuccess(perf_domain_.relative_performance);
}

}  // namespace amlogic_cpu
