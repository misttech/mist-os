// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/cpu/drivers/aml-cpu/aml-cpu-v1.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/platform-defs.h>
#include <lib/mmio/mmio.h>
#include <zircon/syscalls/smc.h>

#include <memory>

#include <ddktl/fidl.h>
#include <fbl/string_buffer.h>

#include "lib/fidl/cpp/wire/channel.h"

namespace amlogic_cpu {

zx_status_t CreateV1Domains(void* context, zx_device_t* parent) {
  zx_status_t st;

  // Get the metadata for the performance domains.
  auto perf_doms = ddk::GetMetadataArray<perf_domain_t>(parent, DEVICE_METADATA_AML_PERF_DOMAINS);
  if (perf_doms.is_error()) {
    zxlogf(ERROR, "Failed to get performance domains from board driver: %s",
           perf_doms.status_string());
    return perf_doms.error_value();
  }

  // Map AOBUS registers
  auto pdev = ddk::PDevFidl::FromFragment(parent);
  if (!pdev.is_valid()) {
    zxlogf(ERROR, "Failed to get platform device fragment");
    return ZX_ERR_NO_RESOURCES;
  }

  auto config = LoadConfiguration(pdev);
  if (config.is_error()) {
    zxlogf(ERROR, "Failed to load cpu configuration: %s", config.status_string());
    return config.error_value();
  }

  auto op_points = ddk::GetMetadataArray<operating_point_t>(parent, config->metadata_type);
  if (!op_points.is_ok()) {
    zxlogf(ERROR, "Failed to get operating point from board driver: %s", op_points.status_string());
    return op_points.error_value();
  }

  // Build and publish each performance domain.
  for (const perf_domain_t& perf_domain : perf_doms.value()) {
    // Vector of operating points that belong to this power domain.
    std::vector<operating_point_t> pd_op_points =
        PerformanceDomainOpPoints(perf_domain, op_points.value());

    auto device = std::make_unique<AmlCpuV1>(parent, std::move(pd_op_points), perf_domain);

    st = device->Init(config.value());
    if (st != ZX_OK) {
      zxlogf(ERROR, "Failed to bind cpu performance domain: %s", zx_status_get_string(st));
      return st;
    }
    [[maybe_unused]] auto ptr = device.release();
  }

  return ZX_OK;
}

zx_status_t AmlCpuV1::Init(const AmlCpuConfiguration& config) {
  fbl::StringBuffer<32> fragment_name;
  fidl::ClientEnd<fuchsia_hardware_clock::Clock> pll_div16_client;
  fidl::ClientEnd<fuchsia_hardware_clock::Clock> cpu_div16_client;
  if (config.has_div16_clients) {
    fragment_name.AppendPrintf("clock-pll-div16-%02d", aml_cpu_.GetDomainId());
    zx::result pll_clock_client =
        DdkConnectFragmentFidlProtocol<fuchsia_hardware_clock::Service::Clock>(
            parent(), fragment_name.c_str());
    if (pll_clock_client.is_error()) {
      zxlogf(ERROR, "Failed to get clock protocol from fragment '%s': %s\n", fragment_name.c_str(),
             pll_clock_client.status_string());
      return pll_clock_client.status_value();
    }
    pll_div16_client = std::move(*pll_clock_client);

    fragment_name.Resize(0);
    fragment_name.AppendPrintf("clock-cpu-div16-%02d", aml_cpu_.GetDomainId());
    zx::result cpu_clock_client =
        DdkConnectFragmentFidlProtocol<fuchsia_hardware_clock::Service::Clock>(
            parent(), fragment_name.c_str());
    if (cpu_clock_client.is_error()) {
      zxlogf(ERROR, "Failed to get clock protocol from fragment '%s': %s\n", fragment_name.c_str(),
             cpu_clock_client.status_string());
      return cpu_clock_client.status_value();
    }
    cpu_div16_client = std::move(*cpu_clock_client);
  }

  fragment_name.Resize(0);
  fragment_name.AppendPrintf("clock-cpu-scaler-%02d", aml_cpu_.GetDomainId());
  zx::result clock_client = DdkConnectFragmentFidlProtocol<fuchsia_hardware_clock::Service::Clock>(
      parent(), fragment_name.c_str());
  if (clock_client.is_error()) {
    zxlogf(ERROR, "Failed to get clock protocol from fragment '%s': %s\n", fragment_name.c_str(),
           clock_client.status_string());
    return clock_client.status_value();
  }
  fidl::ClientEnd<fuchsia_hardware_clock::Clock> cpu_scaler_client{std::move(*clock_client)};

  fidl::ClientEnd<fuchsia_hardware_power::Device> power_client;
  if (config.has_power_client) {
    fragment_name.Resize(0);
    fragment_name.AppendPrintf("power-%02d", aml_cpu_.GetDomainId());
    zx::result client_end_result =
        DdkConnectFragmentFidlProtocol<fuchsia_hardware_power::Service::Device>(
            parent(), fragment_name.c_str());
    if (client_end_result.is_error()) {
      zxlogf(ERROR, "Failed to create power client, st = %s", client_end_result.status_string());
      return client_end_result.error_value();
    }

    power_client = std::move(client_end_result.value());
  }

  zx_status_t st = aml_cpu_.Init(std::move(pll_div16_client), std::move(cpu_div16_client),
                                 std::move(cpu_scaler_client), std::move(power_client));
  if (st != ZX_OK) {
    zxlogf(ERROR, "Failed to initialize device, st = %d", st);
    return st;
  }

  aml_cpu_.SetCpuInfo(config.cpu_version_packed);

  st = DdkAdd(ddk::DeviceAddArgs(aml_cpu_.GetName())
                  .set_flags(DEVICE_ADD_NON_BINDABLE)
                  .set_proto_id(ZX_PROTOCOL_CPU_CTRL)
                  .set_inspect_vmo(aml_cpu_.inspector_.DuplicateVmo()));

  if (st != ZX_OK) {
    zxlogf(ERROR, "DdkAdd failed, st = %d", st);
    return st;
  }

  return st;
}

void AmlCpuV1::DdkRelease() { delete this; }

}  // namespace amlogic_cpu

static constexpr zx_driver_ops_t aml_cpu_driver_ops = []() {
  zx_driver_ops_t result = {};
  result.version = DRIVER_OPS_VERSION;
  result.bind = amlogic_cpu::CreateV1Domains;
  return result;
}();

// clang-format off
ZIRCON_DRIVER(aml_cpu, aml_cpu_driver_ops, "zircon", "0.1");
