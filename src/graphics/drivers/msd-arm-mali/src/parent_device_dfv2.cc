// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "parent_device_dfv2.h"

#include <fidl/fuchsia.hardware.gpu.mali/cpp/driver/wire.h>
#include <fidl/fuchsia.hardware.power/cpp/fidl.h>
#include <lib/magma/platform/zircon/zircon_platform_interrupt.h>
#include <lib/magma/platform/zircon/zircon_platform_mmio.h>
#include <lib/scheduler/role.h>
#include <threads.h>
#include <zircon/threads.h>

ParentDeviceDFv2::ParentDeviceDFv2(
    std::shared_ptr<fdf::Namespace> incoming,
    fidl::WireSyncClient<fuchsia_hardware_platform_device::Device> pdev, config::Config config)
    : incoming_(std::move(incoming)), pdev_(std::move(pdev)), config_(std::move(config)) {}

bool ParentDeviceDFv2::SetThreadRole(const char* role_name) {
  zx_status_t status = fuchsia_scheduler::SetRoleForThisThread(role_name);
  if (status != ZX_OK) {
    return DRETF(false, "Failed to set role, status: %s", zx_status_get_string(status));
  }
  return true;
}

zx::bti ParentDeviceDFv2::GetBusTransactionInitiator() {
  return zx::bti(pdev_.GetBusTransactionInitiator()->release_handle());
}

std::unique_ptr<magma::PlatformMmio> ParentDeviceDFv2::CpuMapMmio(unsigned int index) {
  return pdev_.CpuMapMmio(index);
}

std::unique_ptr<magma::PlatformInterrupt> ParentDeviceDFv2::RegisterInterrupt(unsigned int index) {
  return pdev_.RegisterInterrupt(index);
}

zx::result<fdf::ClientEnd<fuchsia_hardware_gpu_mali::ArmMali>>
ParentDeviceDFv2::ConnectToMaliRuntimeProtocol() {
  auto mali_protocol = incoming_->Connect<fuchsia_hardware_gpu_mali::Service::ArmMali>("mali");
  if (mali_protocol.is_error()) {
    DMESSAGE("Error requesting mali protocol: %s", mali_protocol.status_string());
  }
  return mali_protocol;
}

zx::result<std::vector<fdf_power::PowerElementConfiguration>>
ParentDeviceDFv2::GetPowerConfiguration() {
  // TODO(b/358361345): Use //sdk/lib/driver/platform-device/cpp to retrieve power configuration
  // once it supports it.
  fidl::WireResult result = pdev_.fidl()->GetPowerConfiguration();
  if (!result.ok()) {
    DMESSAGE("Failed to send GetPowerConfiguration request: %s", result.status_string());
    return zx::error(result.status());
  }
  if (result->is_error()) {
    DMESSAGE("Failed to get power configuration: %s", zx_status_get_string(result->error_value()));
    return result->take_error();
  }

  std::vector<fdf_power::PowerElementConfiguration> configs;
  for (const auto& wire : result.value()->config) {
    fuchsia_hardware_power::PowerElementConfiguration natural = fidl::ToNatural(wire);

    zx::result config = fdf_power::PowerElementConfiguration::FromFidl(natural);
    if (config.is_error()) {
      DMESSAGE("Failed to parse power configuration: %s", config.status_string());
      return config.take_error();
    }

    configs.push_back(std::move(config.value()));
  }

  return zx::ok(std::move(configs));
}

// static
std::unique_ptr<ParentDeviceDFv2> ParentDeviceDFv2::Create(std::shared_ptr<fdf::Namespace> incoming,
                                                           config::Config config) {
  auto platform_device =
      incoming->Connect<fuchsia_hardware_platform_device::Service::Device>("pdev");
  if (!platform_device.is_ok()) {
    return DRETP(nullptr, "Error requesting platform device service: %s",
                 platform_device.status_string());
  }
  return std::make_unique<ParentDeviceDFv2>(
      std::move(incoming), fidl::WireSyncClient(std::move(*platform_device)), std::move(config));
}
